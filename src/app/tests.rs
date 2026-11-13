//! Tests for the application shell.
//!
//! Three kinds of check live here, and they fail for different reasons:
//!
//! * the GPUI tests mount the real [`ShelluneView`] in a test window and read the
//!   measured bounds of the elements it rendered, so layout, zoom and click or
//!   keyboard behaviour are observed rather than assumed;
//! * the model tests pin down when a section may look successful;
//! * the design tests check the palette, the copy and the platform conventions
//!   against the numbers written down in `docs/design/02-shell-and-design-system.md`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::prelude::*;
use gpui::{
    ClipboardItem, Entity, IntoElement, Modifiers, MouseButton, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, Size, TestAppContext, VisualTestContext, WindowId, div, point, px, size,
};
use shellune_core::{
    Endpoint, EventTag, ForwardBook, ForwardRule, HostConfig, LocalShell, PaneNode, PaneTarget,
    PanelConfig, PanelSection, Session, SessionId, SessionKind, SessionState, SplitAxis,
    TerminalMode, Transfer, TransferDirection, WorkspaceSnapshot, WorkspaceTab, X11Forwarding,
};
use shellune_ssh::X11Status;
use shellune_storage::{
    ForwardStore, HostBook, HostEdit, HostKey, HostStore, KeymapPreferences, TrustStore,
    WORKSPACE_FILE_NAME, WorkspaceStore,
};
use shellune_terminal::shell::ShellEnvironmentNote;
use shellune_terminal::{BlockStatus, ExitReport, GridSize, TermMode, encode_paste};

use crate::app::actions::{
    CycleTextSize, GoToForwards, GoToHosts, GoToLocalShells, GoToSessions, GoToSettings,
    GoToTransfers, ToggleLanguage,
};
use crate::app::copy::{self, CHINESE, ENGLISH, Locale, Strings};
use crate::app::files::{FileSource, FilesStop};
use crate::app::hosts::HostView;
use crate::app::keymap::Keymap;
use crate::app::layout::Direction;
use crate::app::nav::{Destination, Platform};
use crate::app::product;
use crate::app::remote::{MethodChoice, RemotePanel, RemoteStatus};
use crate::app::terminal::{
    PaneState, SearchNote, SnippetOffer, SourceEvent, TerminalAttention, TerminalPane,
    TerminalShortcuts,
};
use crate::app::theme::{
    self, BODY_CONTRAST_FLOOR, GRAPHIC_CONTRAST_FLOOR, Metrics, Motion, Tone, Zoom, palette,
};
use crate::app::view::{AccentRow, CloseIntent, KeyRowControl, close_intent};
use crate::app::workspace::{
    AttentionKind, AttentionSettings, Failure, PrimaryOutcome, Recovery, Section, SectionState,
    Workspace,
};
use crate::app::{
    DEFAULT_WINDOW_SIZE, MIN_WINDOW_SIZE, ReopenChoice, ShelluneView, key_bindings, reopen_choice,
};

fn design_window() -> Size<Pixels> {
    size(px(DEFAULT_WINDOW_SIZE.0), px(DEFAULT_WINDOW_SIZE.1))
}

fn minimum_window() -> Size<Pixels> {
    size(px(MIN_WINDOW_SIZE.0), px(MIN_WINDOW_SIZE.1))
}

/// A window tall enough to show the whole settings page at once.
///
/// The shortcut rows sit below the fold of the 720×480 minimum window, where a user
/// reaches them by scrolling; a test that clicks them uses this instead.
fn tall_window() -> Size<Pixels> {
    size(px(DEFAULT_WINDOW_SIZE.0), px(2400.0))
}

fn assert_within_width(bounds: gpui::Bounds<Pixels>, viewport: Size<Pixels>, what: &str) {
    assert!(
        bounds.size.width > px(0.0),
        "{what} was measured with an empty box: {bounds:?}"
    );
    assert!(
        bounds.origin.x >= px(-0.5),
        "{what} starts left of the window: {bounds:?}"
    );
    assert!(
        bounds.origin.x + bounds.size.width <= viewport.width + px(0.5),
        "{what} extends past the right edge ({bounds:?} in {viewport:?})"
    );
}

/// Whether `inner` sits inside `outer` on the horizontal axis.
///
/// A dialog clips what it does not fit (`overflow_hidden`), so a row drawn past its
/// card's right edge is drawn past everything a person can reach: the page under a
/// dialog has no horizontal scroll, and at 200% text in the minimum window there is no
/// room to spare. That makes "inside the card" the property to measure, not "inside the
/// window" (R4).
fn assert_inside_horizontally(
    inner: gpui::Bounds<Pixels>,
    outer: gpui::Bounds<Pixels>,
    what: &str,
) {
    assert!(
        inner.size.width > px(0.0),
        "{what} was measured with an empty box: {inner:?}"
    );
    assert!(
        inner.origin.x >= outer.origin.x - px(0.5),
        "{what} starts left of its card: {inner:?} in {outer:?}"
    );
    assert!(
        inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + px(0.5),
        "{what} extends past its card's right edge: {inner:?} in {outer:?}"
    );
}

fn assert_fits_inside(bounds: gpui::Bounds<Pixels>, viewport: Size<Pixels>, what: &str) {
    assert!(
        bounds.size.width > px(0.0) && bounds.size.height > px(0.0),
        "{what} was measured with an empty box: {bounds:?}"
    );
    assert!(
        bounds.origin.x >= px(-0.5) && bounds.origin.y >= px(-0.5),
        "{what} starts outside the window: {bounds:?}"
    );
    assert!(
        bounds.origin.x + bounds.size.width <= viewport.width + px(0.5),
        "{what} extends past the right edge ({bounds:?} in {viewport:?})"
    );
    assert!(
        bounds.origin.y + bounds.size.height <= viewport.height + px(0.5),
        "{what} extends past the bottom edge ({bounds:?} in {viewport:?})"
    );
}

fn bounds_of(cx: &mut VisualTestContext, selector: &'static str) -> gpui::Bounds<Pixels> {
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} was never rendered"))
}

/// The transfer title and count sit above the scroller.
///
/// The scroller used to be the strip itself, so its clip cut the first line of
/// glyphs at 720×480 and 200% text. The title has to clear that clip, and the
/// failure underneath still has to fit the strip.
fn assert_transfer_title_clears_the_scroll_clip(cx: &mut VisualTestContext) {
    let strip = bounds_of(cx, "files.transfers");
    let heading = bounds_of(cx, "transfers.heading");
    let badge = bounds_of(cx, "transfers.count");
    let scroll = bounds_of(cx, "files.transfers.scroll");
    for (name, bounds) in [("transfers.heading", heading), ("transfers.count", badge)] {
        assert!(
            bounds.origin.y >= strip.origin.y - px(0.5)
                && bounds.origin.y + bounds.size.height
                    <= strip.origin.y + strip.size.height + px(0.5),
            "{name} is clipped by the transfer strip: {bounds:?} in {strip:?}"
        );
    }
    assert!(
        scroll.origin.y + px(0.5) >= heading.origin.y + heading.size.height,
        "the title is still inside the scrollport that clips its first line: \
         heading {heading:?} scroll {scroll:?}"
    );
}

fn destination_of(view: &Entity<ShelluneView>, cx: &mut VisualTestContext) -> Destination {
    cx.update(|_window, app| view.read(app).workspace().destination())
}

/// Gives the shell the keyboard, as the window does on open.
///
/// Actions are dispatched along the focus path, so a test that dispatches an
/// action without focusing the shell would be testing a state the application
/// never renders.
fn focus_root(view: &Entity<ShelluneView>, cx: &mut VisualTestContext) {
    let root = cx.update(|_window, app| view.read(app).root_focus().clone());
    cx.update(|window, _app| window.focus(&root));
    cx.run_until_parked();
}

fn connected_local_session(title: &str) -> Session {
    let mut session = Session::new(
        SessionKind::Local {
            shell: Some("pwsh".to_owned()),
        },
        title,
    );
    session.begin_connecting().expect("a new session starts");
    session.connected().expect("a local session connects");
    session
}

// ---------------------------------------------------------------------------
// The session list as a model (R7)
// ---------------------------------------------------------------------------

#[test]
fn a_session_that_ended_can_be_restarted_but_a_live_one_cannot() {
    let mut workspace = Workspace::new();
    let mut ended = connected_local_session("first");
    ended
        .disconnected(shellune_core::SessionFailure::ProcessExited { code: Some(0) })
        .expect("the shell ended");
    let ended_id = ended.id();
    let live = connected_local_session("second");
    let live_id = live.id();
    workspace.set_sessions_for_test(vec![ended, live]);

    assert!(
        workspace.session_reconnecting(ended_id),
        "a session that ended starts another process"
    );
    assert_eq!(
        workspace
            .session(ended_id)
            .map(|session| session.generation()),
        Some(1),
        "the reconnect is the second process of that session"
    );
    assert_eq!(
        workspace
            .session(ended_id)
            .map(|session| session.state().clone()),
        Some(SessionState::Connecting)
    );
    assert!(
        !workspace.session_reconnecting(live_id),
        "a live session is not restarted behind the user's back"
    );
    assert_eq!(
        workspace
            .session(live_id)
            .map(|session| session.state().clone()),
        Some(SessionState::Connected),
        "the other session did not move"
    );
}

#[test]
fn forgetting_a_session_that_never_started_keeps_the_others() {
    let mut workspace = Workspace::new();
    let mut pending = Session::new(SessionKind::Local { shell: None }, "never ran");
    pending.begin_connecting().expect("a new session starts");
    let pending_id = pending.id();
    let live = connected_local_session("running");
    let live_id = live.id();
    workspace.set_sessions_for_test(vec![pending, live]);

    workspace.session_forgotten(pending_id);
    assert!(workspace.session(pending_id).is_none());
    assert_eq!(
        workspace
            .session(live_id)
            .map(|session| session.state().clone()),
        Some(SessionState::Connected),
        "a shell that never ran must not take the running one with it"
    );
}

// ---------------------------------------------------------------------------
// Rendered layout
// ---------------------------------------------------------------------------

/// The controls of the top bar that every page shares.
const TOP_BAR: [&str; 5] = [
    "nav.hosts",
    "header.primary",
    "header.drag",
    "palette.open",
    "nav.settings",
];

#[gpui::test]
async fn the_top_bar_and_the_home_fit_inside_the_design_window(cx: &mut TestAppContext) {
    let (_view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    for selector in TOP_BAR {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    assert_fits_inside(bounds_of(cx, "header.title"), viewport, "home title");
    assert_fits_inside(bounds_of(cx, "shell.picker"), viewport, "local shells");
    assert_fits_inside(bounds_of(cx, "hosts.new"), viewport, "new host");
    assert_fits_inside(bounds_of(cx, "content"), viewport, "content panel");
}

#[gpui::test]
async fn the_minimum_window_still_shows_the_top_bar_and_the_home(cx: &mut TestAppContext) {
    let (_view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_eq!(
        viewport,
        minimum_window(),
        "the test window is not the design minimum"
    );
    for selector in TOP_BAR {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    assert_fits_inside(bounds_of(cx, "content"), viewport, "content panel");
    assert_fits_inside(bounds_of(cx, "hosts.new"), viewport, "new host");
}

#[gpui::test]
async fn one_step_of_text_size_is_115_percent_and_stays_inside_the_minimum_window(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    cx.dispatch_action(CycleTextSize);
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Comfortable,
        "one step from the default is 115%"
    );
    let viewport = cx.update(|window, _app| window.viewport_size());
    for selector in TOP_BAR {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    assert_fits_inside(bounds_of(cx, "content"), viewport, "content panel");
}

#[gpui::test]
async fn two_hundred_percent_chinese_keeps_every_page_inside_the_minimum_window(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    cx.dispatch_action(ToggleLanguage);
    cx.dispatch_action(CycleTextSize);
    cx.dispatch_action(CycleTextSize);
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese
    );
    let viewport = cx.update(|window, _app| window.viewport_size());
    for (page, name) in [
        (Destination::Hosts, "hosts"),
        (Destination::Settings, "settings"),
    ] {
        match page {
            Destination::Settings => cx.dispatch_action(crate::app::actions::GoToSettings),
            _ => cx.dispatch_action(GoToHosts),
        }
        cx.run_until_parked();
        assert_eq!(destination_of(&view, cx), page, "{name}");
        assert_within_width(bounds_of(cx, "header.title"), viewport, name);
        assert_fits_inside(bounds_of(cx, "nav.settings"), viewport, name);
        assert_within_width(bounds_of(cx, "content"), viewport, name);
        if page == Destination::Settings {
            // The page is one item at a time and scrolls at this size, so the settings are
            // reached through the navigation above them: what has to be inside the window is
            // that navigation, and nothing may be wider than the window (R8).
            assert_within_width(bounds_of(cx, "settings.crumb"), viewport, name);
            assert_fits_inside(bounds_of(cx, "settings.picker.category"), viewport, name);
            assert_within_width(bounds_of(cx, "settings.picker.item"), viewport, name);
        } else {
            assert_fits_inside(bounds_of(cx, "header.title"), viewport, name);
        }
    }
}

/// At the minimum window with the largest text, every settings page, the card it draws and
/// every control in that card stay inside the window (R8, R16).
///
/// The page is one item at a time now, so this walks the navigation: the category, then the
/// item, then the box that item's controls were measured in. A control drawn past the card's
/// right edge is drawn past the window too, and the page - it scrolls downwards only - cannot
/// bring it back. The boxes are measured, not photographed, so a control squeezed to a stub
/// still has to be inside the box it belongs to.
/// One row of the settings page table: the item, the selector of the row it draws, and the
/// selectors of the controls in that row.
type SettingsRow<'a> = (&'static str, &'static str, &'a [&'static str]);

/// One category of the settings page table: its selector, and the items inside it.
type SettingsCategory<'a> = (&'static str, &'a [SettingsRow<'a>]);

#[gpui::test]
async fn every_settings_row_and_control_stays_inside_its_card_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    show_settings_at_the_minimum_window_in_large_text(&view, cx);
    let viewport = cx.update(|window, _app| window.viewport_size());
    // The size this test claims to measure: the smallest window the app allows, in logical
    // pixels. A window that came out larger would make every assertion below pass while the
    // real minimum window still overflowed.
    assert_eq!(
        viewport,
        minimum_window(),
        "the settings page is not being measured at the minimum window"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest
    );
    let (metrics, theme_system, unbind_label) = cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        let locale = workspace.locale();
        (
            workspace.metrics(),
            workspace.strings().theme_system,
            copy::ux_for(locale).keys_unbind,
        )
    });
    // The test font draws the theme labels near 100px. A real UI font at this text size is
    // four ems plus the segment's padding, and the measured segment has to reserve that or
    // the same label is cut off at the real window's right edge.
    let theme_floor = crate::app::components::segment_reserved_width(theme_system, metrics);
    let full_em = metrics.label_size() * 4.0 + metrics.scale(theme::spacing::TIGHT + 2.0) * 2.0;
    assert!(
        theme_floor + px(0.5) >= full_em,
        "the theme floor {theme_floor:?} is below a full-em '{theme_system}' ({full_em:?})"
    );

    let content = bounds_of(cx, "content");
    assert!(
        content.size.width <= viewport.width + px(0.5),
        "the settings scroll grew past the window: {content:?} in {viewport:?}"
    );
    let page_pad = metrics.scale(theme::spacing::LOOSE);

    // At this size the two columns do not fit, so the navigation is the two pickers above the
    // page. Both are inside the window, and the column they replaced is not drawn (R8).
    assert!(
        cx.debug_bounds("settings.picker.category").is_some(),
        "the folded navigation does not draw its category picker"
    );
    assert!(cx.debug_bounds("settings.picker.item").is_some());
    assert!(
        cx.debug_bounds("settings.nav").is_none(),
        "the navigation column is drawn although the page folded it"
    );

    // Every item of every category: its page, its card and the controls in that card.
    let mut checked = 0;
    let pages: &[SettingsCategory] = &[
        (
            "settings.category.appearance",
            &[
                (
                    "theme",
                    "settings.row.theme",
                    &["settings.theme.0", "settings.theme.1", "settings.theme.2"][..],
                ),
                (
                    "language",
                    "settings.row.language",
                    &["settings.language.0", "settings.language.1"][..],
                ),
                (
                    "text-size",
                    "settings.row.text-size",
                    &[
                        "settings.zoom.0",
                        "settings.zoom.1",
                        "settings.zoom.2",
                        "settings.zoom.3",
                    ][..],
                ),
                (
                    "motion",
                    "settings.row.motion",
                    &["settings.motion.0", "settings.motion.1"][..],
                ),
            ],
        ),
        (
            "settings.category.terminal",
            &[
                (
                    "scheme",
                    "settings.row.scheme",
                    &["settings.scheme.0", "settings.scheme.1"][..],
                ),
                (
                    "font-size",
                    "settings.row.font-size",
                    &[
                        "settings.font.less",
                        "settings.font.value",
                        "settings.font.more",
                    ][..],
                ),
                ("default-shell", "settings.row.default-shell", &[][..]),
            ],
        ),
        (
            "settings.category.connection",
            &[(
                "policy",
                "settings.row.keepalive-interval",
                &["settings.value.keepalive-interval"][..],
            )],
        ),
        (
            "settings.category.notices",
            &[
                (
                    "bell-notice",
                    "settings.row.bell-notice",
                    &["settings.notice.bell.0", "settings.notice.bell.1"][..],
                ),
                (
                    "finished-notice",
                    "settings.row.finished-notice",
                    &["settings.notice.finished.0", "settings.notice.finished.1"][..],
                ),
            ],
        ),
        (
            "settings.category.data",
            &[
                (
                    "hosts-file",
                    "settings.row.hosts-file",
                    &["settings.paths"][..],
                ),
                ("import", "settings.row.import", &["settings.import"][..]),
            ],
        ),
        (
            "settings.category.about",
            &[("version", "settings.row.about", &["status.version"][..])],
        ),
    ];
    for (category, items) in pages {
        click(cx, category);
        for (item, row, controls) in *items {
            click(cx, item_selector(item));
            let card = card_selector(item);
            let item_name = format!("the {item} card");
            let card_bounds = bounds_of(cx, card);
            assert_within_width(card_bounds, viewport, &item_name);
            assert!(
                card_bounds.origin.x + card_bounds.size.width
                    <= viewport.width - page_pad + px(0.5),
                "{item_name} reaches the window edge instead of keeping the page padding: {card_bounds:?}"
            );
            let row_bounds = bounds_of(cx, row);
            assert_inside_box(row_bounds, card_bounds, row);
            for control in *controls {
                assert_inside_box(bounds_of(cx, control), card_bounds, control);
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 13, "not every settings item was measured");

    // The shortcut page has a card of rows rather than one row: its search, a row, that
    // row's keys and its controls are inside the card as well.
    click(cx, "settings.category.keys");
    click(cx, item_selector("keys"));
    let keys = bounds_of(cx, "settings.keys");
    assert_within_width(keys, viewport, "the shortcut card");
    for part in [
        "settings.keys.search",
        "settings.keys.scope",
        "settings.keys.row.terminal.copy",
        "settings.keys.keycap.terminal.copy.0",
        "settings.keys.unbind.terminal.copy",
    ] {
        assert_inside_box(bounds_of(cx, part), keys, part);
    }
    let unbind_floor = crate::app::components::compact_label_reserved_width(unbind_label, metrics);
    let unbind = bounds_of(cx, "settings.keys.unbind.terminal.copy");
    assert!(
        unbind.size.width + px(0.5) >= unbind_floor,
        "the unbind control is narrower than a full-em font would draw {unbind_label:?}: {unbind:?}, floor {unbind_floor:?}"
    );
}

/// The selector of a settings item in the navigation.
fn item_selector(item: &'static str) -> &'static str {
    match item {
        "theme" => "settings.item.theme",
        "language" => "settings.item.language",
        "text-size" => "settings.item.text-size",
        "motion" => "settings.item.motion",
        "scheme" => "settings.item.scheme",
        "font-size" => "settings.item.font-size",
        "default-shell" => "settings.item.default-shell",
        "policy" => "settings.item.policy",
        "bell-notice" => "settings.item.bell-notice",
        "finished-notice" => "settings.item.finished-notice",
        // The shortcut page is named like its category, so the category is its row.
        "keys" => "settings.category.keys",
        "hosts-file" => "settings.item.hosts-file",
        "import" => "settings.item.import",
        _ => "settings.item.version",
    }
}

/// The card a settings item draws its controls in.
fn card_selector(item: &'static str) -> &'static str {
    match item {
        "theme" => "settings.card.theme",
        "language" => "settings.card.language",
        "text-size" => "settings.card.text-size",
        "motion" => "settings.card.motion",
        "scheme" => "settings.card.scheme",
        "font-size" => "settings.card.font-size",
        "default-shell" => "settings.card.default-shell",
        "policy" => "settings.card.policy",
        "bell-notice" => "settings.card.bell-notice",
        "finished-notice" => "settings.card.finished-notice",
        "hosts-file" => "settings.card.hosts-file",
        "import" => "settings.card.import",
        "keys" => "settings.keys",
        _ => "settings.card.version",
    }
}

/// A control that is wider than its row wraps inside it rather than leaving the window
/// (R8).
///
/// The default-shell choice is the case the walkthrough caught: the names of the shells on
/// this machine are wider than the row at 200% text, and the track used to keep its full
/// width - drawn past the row and past the window's right edge. The shells are fixed here so
/// the test measures the layout and not the machine it runs on.
#[gpui::test]
async fn a_settings_control_wider_than_its_row_wraps_inside_it_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let mut workspace = Workspace::new();
    workspace.set_shells(vec![
        LocalShell::new("Windows PowerShell (x86)", "powershell.exe"),
        LocalShell::new("PowerShell 7 Preview", "pwsh.exe"),
        LocalShell::new("Command Prompt (Administrator)", "cmd.exe"),
    ]);
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
    show_settings_at_the_minimum_window_in_large_text(&view, cx);
    let viewport = cx.update(|window, _app| window.viewport_size());
    click(cx, "settings.category.terminal");
    click(cx, "settings.item.default-shell");

    let card = bounds_of(cx, "settings.card.default-shell");
    assert_within_width(card, viewport, "the default-shell card");
    let row = bounds_of(cx, "settings.row.default-shell");
    assert_inside_box(row, card, "the default-shell row");
    let track = bounds_of(cx, "settings.shell");
    assert_inside_box(track, card, "the default-shell choice");
    assert!(
        track.origin.x >= row.origin.x - px(0.5)
            && track.origin.x + track.size.width <= row.origin.x + row.size.width + px(0.5),
        "the default-shell choice leaves its row: {track:?} in {row:?}"
    );
    assert!(
        track.size.width <= row.size.width,
        "the choice kept its width instead of shrinking to the row: {track:?} in {row:?}"
    );

    // So the shells gain a line inside the same track: the first shell is on the first line
    // and a later one below it, all of them inside the track and the card. Each segment is
    // measured on its own: the track being inside the card says nothing about a segment that
    // hangs over its edge.
    let first = bounds_of(cx, "settings.shell.0");
    let later = bounds_of(cx, "settings.shell.1");
    assert!(
        first.origin.y < later.origin.y,
        "the shells stayed on one line instead of wrapping: {first:?} and {later:?}"
    );
    let metrics = cx.update(|_window, app| view.read(app).workspace().metrics());
    for (selector, name) in [
        ("settings.shell.0", "Windows PowerShell (x86)"),
        ("settings.shell.1", "PowerShell 7 Preview"),
        ("settings.shell.2", "Command Prompt (Administrator)"),
    ] {
        let bounds = bounds_of(cx, selector);
        let floor = crate::app::components::segment_reserved_width(name, metrics);
        assert!(
            bounds.size.width + px(0.5) >= floor,
            "{selector} is narrower than a real font would draw {name:?}: {bounds:?}, floor {floor:?}"
        );
        assert!(
            bounds.origin.x >= track.origin.x - px(0.5)
                && bounds.origin.x + bounds.size.width
                    <= track.origin.x + track.size.width + px(0.5),
            "{selector} leaves the choice it belongs to: {bounds:?} in {track:?}"
        );
        assert_inside_box(bounds, card, selector);
    }
}

/// A shortcut row keeps its keys and its controls inside the page at 200% text (R8).
///
/// A combination as long as a real one takes a wide keycap once the text is doubled, and the
/// row then holds a name column, the key and three buttons. The row wraps those parts instead
/// of letting the buttons leave the window.
#[gpui::test]
async fn the_shortcut_rows_keep_their_keys_and_buttons_inside_the_minimum_window(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    // A long combination, so the keycap is as wide as a real keyboard layout makes it.
    let saved = saved_for_this_platform(vec![shellune_storage::KeymapEntry::new(
        "terminal.copy",
        "terminal",
        Some("ctrl-alt-shift-f12".to_owned()),
    )]);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_saved_keymap_for_test(&saved, cx));
    });
    show_settings_at_the_minimum_window_in_large_text(&view, cx);
    let viewport = cx.update(|window, _app| window.viewport_size());
    click(cx, "settings.category.keys");
    let card = bounds_of(cx, "settings.keys");
    assert_within_width(card, viewport, "the shortcut page");
    let row = "settings.keys.row.terminal.copy";
    assert_inside_box(bounds_of(cx, row), card, row);
    for control in [
        "settings.keys.keycap.terminal.copy.0",
        "settings.keys.change.terminal.copy",
        "settings.keys.unbind.terminal.copy",
        // The combination differs from the default, so this row also offers to go back.
        "settings.keys.reset.terminal.copy",
    ] {
        assert_inside_box(bounds_of(cx, control), card, control);
    }
    let (metrics, key_label) = cx.update(|_window, app| {
        let view = view.read(app);
        (
            view.workspace().metrics(),
            view.keymap()
                .label("terminal.copy")
                .unwrap_or_else(|| "Ctrl+Alt+Shift+F12".to_owned()),
        )
    });
    let keycap = bounds_of(cx, "settings.keys.keycap.terminal.copy.0");
    let key_floor = crate::app::components::keycap_reserved_width(&key_label, metrics);
    assert!(
        keycap.size.width + px(0.5) >= key_floor,
        "the keycap is narrower than a real font would draw {key_label:?}: {keycap:?}, floor {key_floor:?}"
    );
    // The search and the scope filter above the list are controls of the same page and stay
    // inside the card too.
    assert_inside_box(
        bounds_of(cx, "settings.keys.search"),
        card,
        "the search field",
    );
    assert_inside_box(
        bounds_of(cx, "settings.keys.scope"),
        card,
        "the scope filter",
    );
}

/// The selector of a settings page.
fn page_selector(item: &'static str) -> &'static str {
    match item {
        "theme" => "settings.page.theme",
        "language" => "settings.page.language",
        "text-size" => "settings.page.text-size",
        "motion" => "settings.page.motion",
        "scheme" => "settings.page.scheme",
        "font-size" => "settings.page.font-size",
        "default-shell" => "settings.page.default-shell",
        "policy" => "settings.page.policy",
        "bell-notice" => "settings.page.bell-notice",
        "finished-notice" => "settings.page.finished-notice",
        "keys" => "settings.page.keys",
        "hosts-file" => "settings.page.hosts-file",
        "import" => "settings.page.import",
        _ => "settings.page.version",
    }
}

/// Opening a category again shows the item that was open there, and only that item (R16).
#[gpui::test]
async fn going_back_to_a_category_shows_the_item_that_was_open(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    click(cx, "nav.settings");

    // The page opens on the appearance category, on its first item, and the row it draws is
    // the only one: a page per item is the whole point of the second level (R16).
    assert_eq!(
        cx.update(|_window, app| view.read(app).settings_page_id()),
        ("appearance", "theme")
    );
    assert!(cx.debug_bounds("settings.card.theme").is_some());

    // The language is the second item of that category.
    click(cx, "settings.item.language");
    assert_eq!(
        cx.update(|_window, app| view.read(app).settings_page_id()),
        ("appearance", "language")
    );
    assert!(cx.debug_bounds("settings.card.language").is_some());

    // Another category, and back: the appearance category is still on the language.
    click(cx, "settings.category.terminal");
    assert_eq!(
        cx.update(|_window, app| view.read(app).settings_page_id()),
        ("terminal", "scheme")
    );
    click(cx, "settings.category.appearance");
    assert_eq!(
        cx.update(|_window, app| view.read(app).settings_page_id()),
        ("appearance", "language"),
        "the category reopened on its first item instead of the one that was open"
    );
    assert!(cx.debug_bounds("settings.card.language").is_some());
}

/// The shortcut page narrows by action name, by the combination in force and by scope (R16).
#[gpui::test]
async fn the_shortcut_page_searches_by_name_by_combination_and_by_scope(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    click(cx, "settings.category.keys");
    let listed = |cx: &mut VisualTestContext| {
        cx.update(|_window, app| view.read(app).settings_key_rows(app))
    };
    assert!(listed(cx).contains(&"nav.hosts"));
    assert!(listed(cx).contains(&"terminal.copy"));

    // By name.
    type_into(cx, "settings.keys.search", "copy");
    assert!(listed(cx).contains(&"terminal.copy"));
    assert!(
        !listed(cx).contains(&"nav.hosts"),
        "the name search left an action that does not match it"
    );
    clear_field(cx, "settings.keys.search", 4);
    assert!(listed(cx).contains(&"nav.hosts"));

    // By the combination in force, spelled the way a person reads it.
    let combination = Keymap::defaults(Platform::current())
        .keystroke("terminal.copy")
        .expect("copy has a default combination")
        .to_owned();
    let spelled = crate::app::keymap::keystroke_label(&combination, Platform::current());
    type_into(cx, "settings.keys.search", &spelled);
    assert!(
        listed(cx).contains(&"terminal.copy"),
        "the combination in force does not find its own action"
    );
    assert!(!listed(cx).contains(&"nav.hosts"));
    clear_field(cx, "settings.keys.search", spelled.chars().count());

    // By scope: an action of the window is not listed while the scope is the terminal.
    click(cx, "settings.keys.scope.terminal");
    assert!(listed(cx).contains(&"terminal.copy"));
    assert!(
        !listed(cx).contains(&"nav.hosts"),
        "a window action is listed under the terminal scope"
    );
    click(cx, "settings.keys.scope.all");
    assert!(listed(cx).contains(&"nav.hosts"));
}

/// A saved set in which two actions answer one combination is explained on the shortcut page
/// rather than being applied halfway (R16).
#[gpui::test]
async fn a_saved_keymap_in_conflict_is_explained_on_the_shortcut_page(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    let saved = saved_for_this_platform(vec![
        shellune_storage::KeymapEntry::new("nav.hosts", "shell", Some("ctrl-alt-h".to_owned())),
        shellune_storage::KeymapEntry::new("nav.settings", "shell", Some("ctrl-alt-h".to_owned())),
    ]);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_saved_keymap_for_test(&saved, cx));
    });
    cx.run_until_parked();
    click(cx, "settings.category.keys");

    assert!(
        matches!(
            cx.update(|_window, app| view.read(app).keymap_problem().cloned()),
            Some(crate::app::keymap::Problem::Conflict { .. })
        ),
        "the conflict the saved set holds is kept for the page to show"
    );
    assert!(
        cx.debug_bounds("settings.keys.conflicts").is_some(),
        "the shortcut page names the two actions that share a combination"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).keymap().clone()),
        Keymap::defaults(Platform::current()),
        "the defaults are in force instead of a half-applied set"
    );
}

/// The minimum window at 115% text reaches every category and every item by its navigation,
/// and nothing on the way is clipped (R8, R16).
#[gpui::test]
async fn the_minimum_window_reaches_every_settings_category_and_item_at_115_percent(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    cx.dispatch_action(ToggleLanguage);
    cx.dispatch_action(CycleTextSize);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Comfortable,
        "one step of text size is 115%"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese
    );
    cx.dispatch_action(crate::app::actions::GoToSettings);
    cx.run_until_parked();
    let viewport = cx.update(|window, _app| window.viewport_size());

    // Two columns do not fit here, so the navigation is the two pickers above the page: every
    // category and every item is still one press away.
    assert!(cx.debug_bounds("settings.nav").is_none());
    assert!(cx.debug_bounds("settings.picker.category").is_some());
    let mut reached = 0;
    for (category, items) in [
        (
            "settings.category.appearance",
            &["theme", "language", "text-size", "motion"][..],
        ),
        (
            "settings.category.terminal",
            &["scheme", "font-size", "default-shell"][..],
        ),
        ("settings.category.connection", &["policy"][..]),
        (
            "settings.category.notices",
            &["bell-notice", "finished-notice"][..],
        ),
        ("settings.category.keys", &["keys"][..]),
        ("settings.category.data", &["hosts-file", "import"][..]),
        ("settings.category.about", &["version"][..]),
    ] {
        assert_within_width(bounds_of(cx, category), viewport, category);
        click(cx, category);
        for item in items {
            click(cx, item_selector(item));
            assert!(
                cx.debug_bounds(page_selector(item)).is_some(),
                "{item} has no page after its navigation entry was pressed"
            );
            assert_within_width(bounds_of(cx, page_selector(item)), viewport, item);
            assert_within_width(bounds_of(cx, card_selector(item)), viewport, item);
            reached += 1;
        }
    }
    assert_eq!(reached, 14, "not every settings item was reachable");

    // The way out is on the page as it always was, so a person is never stranded here.
    assert_within_width(bounds_of(cx, "nav.hosts"), viewport, "the hosts entry");
}
/// Asserts one measured box stays inside another one, on the left and on the right.
///
/// A settings page that scrolls downwards only cannot reach a control drawn past the
/// card it belongs to, and the card itself cannot leave the window (R4). Both are
/// measured boxes, so this catches a control that overflowed its row, and a row that
/// overflowed its card, without either being clipped on the way.
fn assert_inside_box(inner: gpui::Bounds<Pixels>, outer: gpui::Bounds<Pixels>, what: &str) {
    assert!(
        inner.size.width > px(0.0),
        "{what} was measured with an empty box: {inner:?}"
    );
    assert!(
        inner.origin.x >= outer.origin.x - px(0.5),
        "{what} starts left of the box it belongs to: {inner:?} in {outer:?}"
    );
    assert!(
        inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + px(0.5),
        "{what} leaves the box it belongs to on the right: {inner:?} in {outer:?}"
    );
}

/// Sets the window to the minimum size and the interface to the largest text in Chinese,
/// then opens the settings page — the size at which labels and controls meet.
fn show_settings_at_the_minimum_window_in_large_text(
    view: &Entity<ShelluneView>,
    cx: &mut VisualTestContext,
) {
    cx.simulate_resize(minimum_window());
    focus_root(view, cx);
    cx.dispatch_action(ToggleLanguage);
    largest_text_size(view, cx);
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese
    );
    cx.dispatch_action(GoToSettings);
    cx.run_until_parked();
}

#[gpui::test]
async fn reduced_motion_drops_the_loading_fade_and_keeps_the_state(cx: &mut TestAppContext) {
    let workspace = Workspace::with_sections(
        Section::<Session>::ready(Vec::new()),
        Section::<HostConfig>::ready(Vec::new()),
        Section::<Transfer>::loading(),
    );
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    go(cx, GoToTransfers);

    assert!(
        cx.debug_bounds("state.motion.pulse").is_some(),
        "full motion fades the loading mark in"
    );
    assert!(cx.debug_bounds("state.motion.still").is_none());
    assert!(cx.debug_bounds("state.card").is_some());

    cx.update(|_window, app| view.update(app, |view, cx| view.cycle_motion(cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().motion()),
        Motion::Reduced
    );
    // GPUI keeps a debug selector's last bounds until a later frame overwrites that
    // same selector, so a removed pulse can still be reported. The still mark is new
    // in this frame and is only built when the fade is skipped.
    assert!(
        cx.debug_bounds("state.motion.still").is_some(),
        "reduced motion draws the loading mark without the fade"
    );
    assert!(
        cx.debug_bounds("state.card").is_some(),
        "the loading state stays readable without the fade"
    );
    assert!(
        !theme::motion_allowed(),
        "the draw must be using the reduced-motion preference"
    );
    theme::set_motion(Motion::Full);
}

#[gpui::test]
async fn closing_the_palette_returns_focus_to_the_field_that_opened_it(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    focus_root(&view, cx);
    cx.run_until_parked();

    click(cx, "shell.custom");
    assert!(
        cx.update(|window, app| view.read(app).shell_path_focused(window, app)),
        "the custom shell field takes the keyboard when it opens"
    );

    go(cx, crate::app::actions::OpenPalette);
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
    assert!(
        !cx.update(|window, app| view.read(app).shell_path_focused(window, app)),
        "the palette takes the keyboard while it is open"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).palette_open()));
    assert!(
        cx.update(|window, app| view.read(app).shell_path_focused(window, app)),
        "closing the palette returns the keyboard to the field that opened it"
    );
}

#[gpui::test]
async fn clicking_a_navigation_entry_switches_the_current_object(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);

    let bounds = bounds_of(cx, Destination::Hosts.debug_selector());
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    assert_eq!(destination_of(&view, cx), Destination::Hosts);
    assert!(
        cx.update(|window, app| view
            .read(app)
            .destination_focus(Destination::Hosts)
            .is_focused(window)),
        "clicking a navigation entry must move keyboard focus with the selection"
    );
}

#[gpui::test]
async fn keyboard_shortcuts_move_between_pages_and_open_the_tools(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    focus_root(&view, cx);

    let settings = Destination::Settings.keystroke(Platform::current());
    cx.simulate_keystrokes(&settings);
    cx.run_until_parked();
    assert_eq!(
        destination_of(&view, cx),
        Destination::Settings,
        "the {settings} shortcut did not reach the shell"
    );

    cx.dispatch_action(GoToHosts);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Hosts);

    // The old session list is the tab strip: asking for it lands on the terminals.
    cx.dispatch_action(GoToSessions);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);

    // Transfers and forwards are tools over the page, not pages.
    cx.dispatch_action(GoToTransfers);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    assert!(cx.debug_bounds("transfers.dialog").is_some());
}

#[gpui::test]
async fn language_and_text_size_shortcuts_are_wired(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    focus_root(&view, cx);
    cx.dispatch_action(ToggleLanguage);
    cx.run_until_parked();
    cx.dispatch_action(CycleTextSize);
    cx.run_until_parked();

    let (locale, zoom) = cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        (workspace.locale(), workspace.zoom())
    });
    assert_eq!(locale, Locale::Chinese);
    assert_eq!(
        zoom,
        Zoom::Comfortable,
        "the text size control must step up once"
    );
}

// ---------------------------------------------------------------------------
// Honest states
// ---------------------------------------------------------------------------

#[gpui::test]
async fn transfers_and_forwards_open_over_the_page_and_close_again(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    focus_root(&view, cx);

    go(cx, GoToTransfers);
    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_fits_inside(bounds_of(cx, "transfers.dialog"), viewport, "transfers");
    assert_fits_inside(bounds_of(cx, "state.card"), viewport, "no transfer yet");
    click(cx, "transfers.dialog.close");
    assert_eq!(
        cx.update(|_window, app| view.read(app).tool_for_test()),
        None,
        "the close button closes the tool"
    );

    go(cx, GoToForwards);
    assert_fits_inside(bounds_of(cx, "forwards.dialog"), viewport, "forwards");
    assert!(
        cx.debug_bounds("forwards.empty").is_some(),
        "no rule yet says what a tunnel is"
    );
    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(
            workspace.notice(),
            None,
            "nothing failed, so nothing is noticed"
        );
        assert_eq!(workspace.state(Destination::Transfers), SectionState::Empty);
    });
}

#[gpui::test]
async fn every_empty_loading_and_failed_state_renders_a_state_card(cx: &mut TestAppContext) {
    // No capability of this build is "planned" any more, so the states a section can
    // really be in are loading, failed and empty; all three go through one state card.
    let cases = [
        (
            "loading",
            Destination::Transfers,
            Workspace::with_sections(
                Section::<Session>::ready(Vec::new()),
                Section::<HostConfig>::ready(Vec::new()),
                Section::<Transfer>::loading(),
            ),
            SectionState::Loading,
        ),
        (
            "failed",
            Destination::Transfers,
            Workspace::with_sections(
                Section::<Session>::ready(Vec::new()),
                Section::<HostConfig>::ready(Vec::new()),
                Section::<Transfer>::failed(Failure::new(
                    "the transfer list could not be read",
                    Recovery::Retry,
                )),
            ),
            SectionState::Failed,
        ),
        (
            "empty",
            Destination::Hosts,
            Workspace::with_sections(
                Section::<Session>::ready(Vec::new()),
                Section::<HostConfig>::ready(Vec::new()),
                Section::<Transfer>::ready(Vec::new()),
            ),
            SectionState::Empty,
        ),
    ];

    for (name, destination, mut workspace, expected) in cases {
        workspace.select(destination);
        let (view, cx) =
            cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
        cx.simulate_resize(design_window());
        cx.run_until_parked();
        if destination == Destination::Transfers {
            go(cx, GoToTransfers);
        }

        let viewport = cx.update(|window, _app| window.viewport_size());
        assert_eq!(
            cx.update(|_window, app| view.read(app).workspace().state(destination)),
            expected,
            "the {name} fixture did not keep its state"
        );
        // An empty host list is the home page's first-run card, which says what a
        // host is and offers the ways to add one.
        let card = if destination == Destination::Hosts && expected == SectionState::Empty {
            "hosts.empty"
        } else {
            "state.card"
        };
        assert_fits_inside(bounds_of(cx, card), viewport, name);
        assert!(
            cx.debug_bounds("row.0").is_none(),
            "the {name} state must not render list rows"
        );
    }
}

#[gpui::test]
async fn a_host_file_that_cannot_be_read_stays_failed_when_it_is_retried(cx: &mut TestAppContext) {
    // A data directory that is a file cannot hold a host file. Retrying reads the
    // same impossible path, so the failure has to survive the retry instead of
    // turning into an empty host list.
    let dir = TempDir::new("unreadable");
    let root = dir.path().join("data");
    std::fs::write(&root, "not a directory").expect("the sentinel file is written");

    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let store = HostStore::at(&root);
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_store(cx, workspace, store));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).workspace().state(Destination::Hosts),
            SectionState::Failed
        );
    });
    assert!(
        cx.debug_bounds("state.card").is_some(),
        "the failure has to be visible where the hosts would be"
    );

    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(workspace.state(Destination::Hosts), SectionState::Failed);
        assert_eq!(workspace.item_count(Destination::Hosts), 0);
        let reason = workspace
            .failure(Destination::Hosts)
            .map(Failure::reason)
            .unwrap_or_default()
            .to_owned();
        assert!(
            reason.contains("hosts.json"),
            "the reason names the file it could not read: {reason}"
        );
    });
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[test]
fn only_a_ready_section_with_items_can_look_successful() {
    let states = [
        (Section::<u8>::loading(), SectionState::Loading),
        (
            Section::<u8>::failed(Failure::new("boom", Recovery::Retry)),
            SectionState::Failed,
        ),
        (Section::<u8>::ready(Vec::new()), SectionState::Empty),
        (Section::<u8>::ready(vec![1]), SectionState::Populated),
    ];

    for (section, expected) in states {
        assert_eq!(section.state(), expected);
        assert_eq!(
            section.state().is_success(),
            expected == SectionState::Populated,
            "{expected:?} reported the wrong success flag"
        );
    }
}

#[test]
fn local_and_all_session_destinations_stay_distinguishable() {
    let mut remote = Session::new(
        SessionKind::Remote {
            host: shellune_core::HostId::new(),
        },
        "prod web",
    );
    remote.begin_connecting().expect("starts");
    let workspace = Workspace::with_sections(
        Section::ready(vec![remote]),
        Section::<HostConfig>::ready(Vec::new()),
        Section::<Transfer>::ready(Vec::new()),
    );

    assert_eq!(
        workspace.state(Destination::Sessions),
        SectionState::Populated
    );
    assert_eq!(
        workspace.state(Destination::LocalShells),
        SectionState::Empty
    );
    assert_eq!(workspace.item_count(Destination::LocalShells), 0);
    assert_eq!(workspace.item_count(Destination::Sessions), 1);
}

#[test]
fn an_unimplemented_action_reports_that_nothing_started() {
    let mut workspace = Workspace::new();
    // The transfers page is a real, empty result now, and it has no primary action of
    // its own: asking for one must report that nothing started rather than pretend a
    // transfer was queued.
    workspace.select(Destination::Transfers);
    assert_eq!(workspace.request_primary(), PrimaryOutcome::Unavailable);
    assert_eq!(
        workspace.notice(),
        Some(crate::app::workspace::Notice::UnavailableBuild)
    );
    assert_eq!(workspace.item_count(Destination::Transfers), 0);
    assert_eq!(workspace.state(Destination::Transfers), SectionState::Empty);

    // The session list is implemented but real and empty: it says so instead of
    // claiming the capability is missing or showing a session that does not exist.
    workspace.select(Destination::Sessions);
    assert_eq!(workspace.state(Destination::Sessions), SectionState::Empty);
    assert_eq!(workspace.request_primary(), PrimaryOutcome::Unavailable);
    assert_eq!(workspace.item_count(Destination::Sessions), 0);
}

#[test]
fn only_the_implemented_destinations_can_act() {
    let mut workspace = Workspace::new();
    workspace.select(Destination::LocalShells);
    assert_eq!(workspace.request_primary(), PrimaryOutcome::Delegated);
    // Saving a host is implemented, so its action is offered; the store answers
    // whether the write worked rather than the button guessing.
    assert!(workspace.can_act(Destination::Hosts));
    assert!(!workspace.can_act(Destination::Transfers));
    // Nothing was started by the request itself: the view owns the process.
    assert_eq!(workspace.item_count(Destination::LocalShells), 0);
}

#[test]
fn selecting_another_destination_clears_a_stale_notice() {
    let mut workspace = Workspace::new();
    workspace.select(Destination::Transfers);
    assert_eq!(workspace.request_primary(), PrimaryOutcome::Unavailable);
    assert!(workspace.notice().is_some());

    workspace.select(Destination::Sessions);
    assert_eq!(workspace.notice(), None);
    workspace.dismiss_notice();
    assert_eq!(workspace.notice(), None);
}

#[test]
fn text_size_steps_through_every_level_and_wraps() {
    assert_eq!(Zoom::default(), Zoom::Regular);
    assert_eq!(Zoom::Compact.next(), Zoom::Regular);
    assert_eq!(Zoom::Regular.next(), Zoom::Comfortable);
    assert_eq!(Zoom::Comfortable.next(), Zoom::Largest);
    assert_eq!(Zoom::Largest.next(), Zoom::Compact);
    assert_eq!(Zoom::Largest.factor(), 2.0);
    assert_eq!(Zoom::Largest.label(), "200%");
    assert!(Zoom::Comfortable.factor() < Zoom::Largest.factor());

    let metrics = Metrics::new(Zoom::Largest);
    assert!(metrics.body_size() > Metrics::new(Zoom::Comfortable).body_size());
    assert!(metrics.rail_width() > Metrics::new(Zoom::Comfortable).rail_width());
}

// ---------------------------------------------------------------------------
// Copy and platform conventions
// ---------------------------------------------------------------------------

macro_rules! string_fields {
    ($strings:expr, [$($field:ident),* $(,)?]) => {
        vec![$( (stringify!($field), $strings.$field) ),*]
    };
}

fn rendered_strings(strings: &Strings) -> Vec<(&'static str, &'static str)> {
    string_fields!(
        strings,
        [
            nav_local,
            nav_local_summary,
            nav_hosts,
            nav_hosts_summary,
            nav_sessions,
            nav_sessions_summary,
            nav_transfers,
            nav_transfers_summary,
            empty_local_title,
            empty_local_body,
            empty_hosts_title,
            empty_hosts_body,
            empty_sessions_title,
            empty_sessions_body,
            empty_transfers_title,
            empty_transfers_body,
            loading_title,
            loading_body,
            failed_title,
            recovery_label,
            recovery_retry,
            recovery_create_host,
            recovery_import_config,
            recovery_choose_shell,
            next_step,
            primary_hosts,
            unavailable_notice,
            shell_default,
            shell_custom_hint,
            count_hosts,
            local_shell_fallback,
            shell_custom_button,
            shell_custom_placeholder,
            shell_none_found,
            terminal_live,
            terminal_failed,
            terminal_exited,
            terminal_terminated,
            terminal_environment_unavailable,
            terminal_environment_unreadable,
            terminal_environment_locale_replaced,
            terminal_environment_locale_added,
            terminal_copy,
            hosts_search_placeholder,
            keys_search_placeholder,
            hosts_view_all,
            hosts_view_groups,
            hosts_view_recent,
            hosts_recent_empty,
            hosts_no_match,
            hosts_ungrouped,
            host_field_name,
            host_field_address,
            host_field_port,
            host_field_user,
            host_field_group,
            host_editor_new,
            host_editor_edit,
            host_save,
            host_cancel,
            host_edit,
            host_delete,
            host_delete_question,
            host_delete_confirm,
            host_delete_note,
            hosts_saved,
            hosts_deleted,
            hosts_storage_label,
            hosts_retry,
            host_last_connected,
            host_connection_count,
            import_open,
            import_title,
            import_path_label,
            import_path_placeholder,
            import_preview,
            import_source_label,
            import_read_only,
            import_limits_title,
            import_limits_none,
            import_outcome_ready,
            import_outcome_review,
            import_outcome_blocked,
            import_not_offered,
            import_ignored_label,
            import_include_label,
            import_match_label,
            import_nothing,
            import_apply,
            import_applied,
            import_review_count,
            import_skipped,
            import_error_title,
            import_path_required,
            session_kind_local,
            session_kind_remote,
            session_close,
            session_reconnect,
            session_reconnect_no_shell,
            session_reconnect_missing_host,
            files_open,
            files_up,
            files_refresh,
            files_parent_row,
            files_loading,
            files_empty,
            files_unknown_path,
            files_failed,
            files_terminal_note,
            files_refresh_source,
            files_show_remote,
            files_to_remote,
            files_to_local,
            files_delete_recursive_local,
            files_delete_file_local,
            files_change_pending_local,
            files_change_done_local,
            files_change_failed_local,
            files_local_not_found,
            files_local_denied,
            files_local_not_directory,
            files_local_lost,
            files_local_refused,
            files_local_exists,
            files_local_unavailable,
            files_local_root,
            transfers_resume,
            transfers_restart,
            transfers_resume_from,
            transfers_resume_source,
            transfers_resume_unverifiable,
            transfers_resume_unsupported,
            transfers_resume_damaged,
            transfers_close_app_question,
            transfers_close_app_cancel,
            transfers_close_app_confirm,
            transfers_host,
            files_kind_directory,
            files_kind_file,
            files_kind_link,
            files_kind_other,
            palette_placeholder,
            palette_empty,
            palette_group_sessions,
            palette_group_shells,
            palette_group_hosts,
            palette_group_navigate,
            palette_group_actions,
            palette_group_snippets,
            palette_close_tab,
            palette_language,
            palette_text_size,
            palette_restore_tab_title,
            palette_clear_tab_accent,
            palette_current,
            palette_key_move,
            palette_key_run,
            palette_key_close,
            tab_menu_duplicate,
            tab_menu_close,
            tab_menu_close_others,
            tab_menu_close_right,
            theme_system,
            theme_light,
            theme_dark,
            palette_theme,
            motion_full,
            motion_reduced,
            palette_motion,
            quick_connect,
            quick_connect_hint,
            quick_connect_title,
            quick_save_host,
            quick_saved,
            quick_unsaved_note,
            attempt_others,
            attempt_cancel,
            palette_group_recent,
            quick_port_invalid,
            quick_port_blank,
            quick_fix_port,
            quick_invalid_name,
            quick_invalid_address,
            quick_invalid_address_space,
            quick_invalid_user,
            quick_saved_next,
            quick_save_write_failed,
            quick_save_write_next,
            quick_save_duplicate,
            quick_save_duplicate_next,
            quick_save_unknown,
            quick_save_unknown_next,
            quick_save_inconsistent,
            quick_save_inconsistent_next,
            host_auth_title,
            host_secret_passphrase,
            host_remember_password,
            host_remember_passphrase,
            host_remember_note,
            host_secret_saved_placeholder,
            host_secret_failed,
            host_startup_label,
            host_startup_placeholder,
            host_startup_note,
            host_agent_note,
            host_save_connect,
            host_dialog_close,
            remote_saved_secret_failed,
            host_jump_title,
            host_jump_empty,
            host_jump_add,
            host_jump_up,
            host_jump_down,
            host_jump_remove,
            host_proxy_label,
            host_proxy_placeholder,
            host_proxy_note,
            host_proxy_conflict,
            remote_stage_title,
            remote_target_label,
            remote_waiting,
            remote_wait_identity,
            remote_cancel_attempt,
            remote_asking_hop,
            remote_proxy_title,
            remote_proxy_confirm,
            remote_proxy_refuse,
            import_proxy_label,
            import_jump_label,
            import_forward_label,
            import_forwards_failed,
            pane_split_right,
            pane_split_down,
            pane_zoom,
            pane_unzoom,
            pane_active,
            pane_pending,
            pane_pending_body,
            pane_connect,
            pane_connect_all,
            pane_remove,
            pane_close,
            pane_resize_less,
            pane_resize_more,
            tab_menu_move_left,
            tab_menu_move_right,
            pane_missing_host,
            pane_missing_next,
            workspace_dangling_tab,
            workspace_dangling_pane,
            pane_mode_classic,
            pane_mode_integrated,
            terminal_mode_classic,
            terminal_mode_blocks,
            terminal_block_running,
            terminal_block_unknown,
            terminal_block_exit,
            terminal_block_copy,
            terminal_block_rerun,
            terminal_search,
            terminal_search_empty,
            terminal_search_invalid,
            terminal_search_next,
            terminal_search_previous,
            terminal_search_case,
            terminal_search_regex,
            terminal_search_close,
            terminal_search_keys,
            terminal_search_keyboard_terminal,
            terminal_paste_title,
            terminal_paste_target,
            terminal_paste_confirm,
            terminal_paste_cancel,
            terminal_paste_lines,
            terminal_paste_runs,
            terminal_paste_run_confirm,
            terminal_paste_safe,
            terminal_no_blocks,
            terminal_blocks_failed,
            terminal_filter_all,
            terminal_filter_running,
            terminal_filter_unknown,
            terminal_filter_failed,
            terminal_filter_zero,
            nav_forwards,
            nav_forwards_summary,
            empty_forwards_title,
            empty_forwards_body,
            forward_saved,
            forward_starting,
            forward_listening,
            forward_stopped,
            forward_failed,
            forward_start,
            forward_stop,
            forward_save,
            forward_delete,
            forward_edit,
            forward_exposure_title,
            forward_exposure_all,
            forward_exposure_named,
            forward_exposure_confirm,
            forward_exposure_cancel,
            forward_close_rules,
            forward_close_app,
            forward_open,
            forward_carrier_lost,
            forward_connect_only,
            field_cut,
            field_copy,
            field_paste,
            field_select_all,
            field_reveal,
            field_conceal,
            field_browse,
            session_menu_open,
            session_menu_reopen,
            session_menu_close,
            session_menu_remove,
            session_menu_remove_closed,
            session_more,
            session_closed_summary,
            tab_menu_rename,
            tab_rename_title,
            tab_rename_placeholder,
            tab_rename_hint,
            tab_rename_save,
            tab_rename_empty,
            tab_rename_auto,
            tab_rename_limit,
            tab_rename_shortened,
            tab_accent_label,
            tab_accent_none,
            tab_accent_iris,
            tab_accent_violet,
            tab_accent_teal,
            tab_accent_lime,
            tab_accent_orchid,
            tab_accent_clay,
            tab_list_title,
            tab_list_hint,
        ]
    )
}

#[test]
fn both_languages_are_complete_and_actually_translated() {
    for (locale, strings) in [(Locale::English, &ENGLISH), (Locale::Chinese, &CHINESE)] {
        for (name, value) in rendered_strings(strings) {
            assert!(!value.trim().is_empty(), "{name} is empty for {locale:?}");
        }
    }

    let english = rendered_strings(&ENGLISH);
    let chinese = rendered_strings(&CHINESE);
    for ((name, english), (_, chinese)) in english.iter().zip(chinese.iter()) {
        assert_ne!(
            english, chinese,
            "{name} is identical in both languages, so the Chinese table is a copy"
        );
        assert!(
            chinese
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{name} has no Chinese characters: {chinese}"
        );
    }
}

#[test]
fn product_copy_matches_the_requirement() {
    assert_eq!(product::NAME, "Shellune");
    assert_eq!(product::PACKAGE_NAME, "shellune");
    assert_eq!(
        product::TAGLINE_EN,
        "A modern terminal for local shells, SSH, and SFTP."
    );
    assert_eq!(product::TAGLINE_ZH, "连接远程，也专注本地。");
    assert_eq!(
        locale_for_tagline(),
        (product::TAGLINE_EN, product::TAGLINE_ZH)
    );
}

fn locale_for_tagline() -> (&'static str, &'static str) {
    (product::TAGLINE_EN, product::TAGLINE_ZH)
}

#[test]
fn shortcuts_follow_each_platform_convention() {
    // Only the three pages have a number: hosts, terminals, settings. The readable
    // label comes from the catalogue, which is the one place a shortcut is written
    // down (R5).
    let windows = Keymap::defaults(Platform::Windows);
    let macos = Keymap::defaults(Platform::MacOs);
    let linux = Keymap::defaults(Platform::Linux);
    assert_eq!(windows.label("nav.hosts").as_deref(), Some("Ctrl+1"));
    assert_eq!(linux.label("nav.local").as_deref(), Some("Ctrl+2"));
    assert_eq!(macos.label("nav.settings").as_deref(), Some("⌘3"));
    assert_eq!(
        windows.label("nav.settings").as_deref(),
        Some("Ctrl+3"),
        "the first default is the one a hint shows; the comma alias is still bound"
    );
    assert_eq!(windows.keystrokes("nav.settings").len(), 2);
    assert_eq!(
        windows.label("tab.select.1").as_deref(),
        Some("Alt+1"),
        "tab jumps take the next free modifier"
    );
    assert_eq!(macos.label("tab.select.1").as_deref(), Some("⌃1"));
    assert_eq!(
        macos.label("palette.open").as_deref(),
        Some("⇧⌘P"),
        "the macOS label puts the symbols in the order users read them"
    );
    assert_eq!(Destination::Hosts.keystroke(Platform::Windows), "ctrl-1");
    assert_eq!(Destination::Settings.keystroke(Platform::MacOs), "cmd-3");
    assert_eq!(Destination::Hosts.next(), Destination::LocalShells);
    assert_eq!(Destination::Settings.next(), Destination::Hosts);
    assert_eq!(Destination::Hosts.previous(), Destination::Settings);
    for page in Destination::PAGES {
        assert!(page.is_page());
        assert_eq!(page.next().previous(), page);
    }
    for destination in Destination::ALL {
        assert_eq!(Destination::ALL[destination.index()], destination);
    }
    assert!(!Destination::Transfers.is_page());
}

#[test]
fn key_bindings_cover_every_destination_plus_the_shell_controls() {
    let bindings = key_bindings(Platform::current());
    assert_eq!(
        bindings.len(),
        Destination::PAGES.len()
            + 1
            + 6
            + 4
            + crate::app::TAB_JUMP_COUNT
            + 2
            + 1
            + 4
            + 4
            + 4
            + 2
            + 2
            + 3,
        "three pages and the settings comma, next and previous, zoom, new shell, language \
         and dismiss; the palette, close tab, next and previous tab; one jump per \
         tab digit; split right and split down, zoom pane, focus, resize and move in \
         four directions, move tab left and right; the keyboard's upload and download; \
         and the three terminal actions (search, copy, paste)"
    );
}

/// The tab and palette shortcuts have to work while a terminal has the keyboard, and
/// none of them may take a key a shell program reads on its own.
#[test]
fn the_tab_and_palette_shortcuts_reach_past_a_terminal() {
    for platform in [Platform::Windows, Platform::Linux, Platform::MacOs] {
        let bindings = key_bindings(platform);
        let (next, previous) = platform.tab_cycle_keystrokes();
        for keystroke in [
            platform.palette_keystroke(),
            platform.close_tab_keystroke(),
            next,
            previous,
        ] {
            let wanted = gpui::Keystroke::parse(keystroke)
                .expect("a valid keystroke")
                .unparse();
            let binding = bindings
                .iter()
                .find(|binding| {
                    binding
                        .keystrokes()
                        .iter()
                        .map(|keystroke| keystroke.unparse())
                        .collect::<Vec<_>>()
                        .join(" ")
                        == wanted
                })
                .unwrap_or_else(|| panic!("{keystroke} is not bound on {platform:?}"));
            let predicate = format!("{:?}", binding.predicate());
            assert!(
                !predicate.contains("Not"),
                "{keystroke} must reach past a terminal, got {predicate}"
            );
        }
    }
    // Ctrl+W and Ctrl+P are shell keys (delete word, previous command) on Windows and
    // Linux; only the shifted forms may be taken there.
    for platform in [Platform::Windows, Platform::Linux] {
        assert_eq!(platform.close_tab_keystroke(), "ctrl-shift-w");
        assert_eq!(platform.palette_keystroke(), "ctrl-shift-p");
    }
}

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

#[test]
fn body_text_reaches_the_contrast_floor_on_every_surface() {
    let surfaces = [
        ("canvas", palette::CANVAS),
        ("surface", palette::SURFACE),
        ("raised", palette::SURFACE_RAISED),
        ("selected", palette::SURFACE_SELECTED),
    ];
    let text = [
        ("primary", palette::TEXT_PRIMARY),
        ("secondary", palette::TEXT_SECONDARY),
        ("muted", palette::TEXT_MUTED),
        ("accent", palette::ACCENT),
    ];

    for (surface_name, surface) in surfaces {
        for (text_name, token) in text {
            let ratio = theme::contrast::ratio(token, surface);
            assert!(
                ratio >= BODY_CONTRAST_FLOOR,
                "{text_name} on {surface_name} is {ratio:.2}:1, below {BODY_CONTRAST_FLOOR}"
            );
        }
    }
}

#[test]
fn host_avatar_initials_are_readable_on_their_tint() {
    for (tint, text) in palette::AVATARS {
        let ratio = theme::contrast::ratio(text, tint);
        assert!(
            ratio >= BODY_CONTRAST_FLOOR,
            "avatar {text:#08x} on {tint:#08x} is {ratio:.2}:1"
        );
    }
}

#[test]
fn tab_identity_colours_are_readable_distinct_and_not_status_hues() {
    let surfaces = [
        palette::CANVAS,
        palette::SURFACE,
        palette::SURFACE_RAISED,
        palette::SURFACE_SELECTED,
    ];
    for token in palette::TAB_ACCENTS {
        for surface in surfaces {
            let dark = theme::contrast::ratio(token, surface);
            assert!(
                dark >= GRAPHIC_CONTRAST_FLOOR,
                "tab colour {token:06x} on {surface:06x} is {dark:.2}:1"
            );
            let light = theme::contrast::ratio(light(token), light(surface));
            assert!(
                light >= GRAPHIC_CONTRAST_FLOOR,
                "light tab colour {token:06x} on {surface:06x} is {light:.2}:1"
            );
        }
    }
    let mut tokens = palette::TAB_ACCENTS.to_vec();
    let count = tokens.len();
    tokens.sort_unstable();
    tokens.dedup();
    assert_eq!(tokens.len(), count, "two tab colours share a value");
    // A tab colour only says which tab this is; it must never be read as a status.
    for token in palette::TAB_ACCENTS {
        for tone in [Tone::Positive, Tone::Warning, Tone::Danger, Tone::Accent] {
            assert_ne!(
                token,
                tone.token(),
                "{token:06x} is also the {tone:?} status colour"
            );
        }
    }
    for accent in shellune_core::TabAccent::ALL {
        let token = theme::tab_accent_token(accent);
        assert!(
            palette::TAB_ACCENTS.contains(&token),
            "{accent:?} has a colour of its own"
        );
        assert_ne!(
            copy::tab_accent_label(accent, Locale::English),
            copy::tab_accent_label(accent, Locale::Chinese),
            "{accent:?} is named in both languages"
        );
    }
}

#[test]
fn status_colours_are_readable_and_distinct() {
    let surfaces = [
        palette::CANVAS,
        palette::SURFACE,
        palette::SURFACE_RAISED,
        palette::SURFACE_SELECTED,
    ];
    let tones = [
        Tone::Neutral,
        Tone::Accent,
        Tone::Positive,
        Tone::Warning,
        Tone::Danger,
        Tone::Planned,
    ];

    for tone in tones {
        for surface in surfaces {
            let ratio = theme::contrast::ratio(tone.token(), surface);
            assert!(
                ratio >= BODY_CONTRAST_FLOOR,
                "{tone:?} on {surface:#08x} is {ratio:.2}:1"
            );
        }
    }

    let mut tokens: Vec<u32> = tones.iter().map(|tone| tone.token()).collect();
    let count = tokens.len();
    tokens.sort_unstable();
    tokens.dedup();
    assert_eq!(tokens.len(), count, "two tones share a colour");

    let ring = theme::contrast::ratio(palette::ACCENT, palette::CANVAS);
    assert!(
        ring >= GRAPHIC_CONTRAST_FLOOR,
        "the focus ring is {ring:.2}:1 against the canvas"
    );
}

#[test]
fn a_live_session_is_the_only_positive_session_state() {
    let states = [
        SessionState::Idle,
        SessionState::Connecting,
        SessionState::VerifyingHost,
        SessionState::Authenticating,
        SessionState::Connected,
        SessionState::Disconnected(shellune_core::SessionFailure::ConnectionLost),
        SessionState::Failed(shellune_core::SessionFailure::AuthenticationFailed),
        SessionState::Closed,
    ];

    for state in states {
        let tone = Tone::for_session_state(&state);
        if state == SessionState::Connected {
            assert_eq!(tone, Tone::Positive);
        } else {
            assert_ne!(tone, Tone::Positive, "{state:?} must not look live");
        }
    }

    // A clean exit is not an error, and a non-zero exit is.
    assert_eq!(
        Tone::for_session_state(&SessionState::Disconnected(
            shellune_core::SessionFailure::ProcessExited { code: Some(0) }
        )),
        Tone::Neutral
    );
    assert_eq!(
        Tone::for_session_state(&SessionState::Disconnected(
            shellune_core::SessionFailure::ProcessExited { code: Some(2) }
        )),
        Tone::Danger
    );

    // The same rule for a reason without a state: the notice above a finished terminal and
    // the session bar both take their tone from it, so a clean exit is never drawn as an
    // error (R12).
    assert_eq!(
        Tone::for_failure(&shellune_core::SessionFailure::ProcessExited { code: Some(0) }),
        Tone::Neutral
    );
    assert_eq!(
        Tone::for_failure(&shellune_core::SessionFailure::ProcessExited { code: Some(2) }),
        Tone::Danger
    );
    // A shell that never got going but exited cleanly is not a failure either.
    assert_eq!(
        Tone::for_session_state(&SessionState::Failed(
            shellune_core::SessionFailure::ProcessExited { code: Some(0) }
        )),
        Tone::Neutral
    );
    let mut transfer = Transfer::queued(
        shellune_core::SessionId::new(),
        TransferDirection::Upload,
        "a.txt",
        "/tmp/a.txt",
    );
    assert_ne!(Tone::for_transfer_state(transfer.state()), Tone::Positive);
    transfer
        .complete(3)
        .expect_err("queued transfers cannot complete");
    assert_ne!(Tone::for_transfer_state(transfer.state()), Tone::Positive);
}

#[test]
fn a_host_without_a_group_shows_no_group_chip() {
    let mut host = HostConfig::new("web", "10.0.0.5", 22, "deploy").expect("a valid host");
    assert_eq!(crate::app::view::host_group_chip(&host), None);
    host.set_group(Some("  "));
    assert_eq!(
        crate::app::view::host_group_chip(&host),
        None,
        "a blank group is not a group"
    );
    host.set_group(Some("Production"));
    assert_eq!(
        crate::app::view::host_group_chip(&host).as_deref(),
        Some("Production")
    );
}

// ---------------------------------------------------------------------------
// Local terminal (R2, R8)
// ---------------------------------------------------------------------------

/// Mounts a bare terminal pane so the input path and the grid can be driven
/// without going through the whole shell.
struct PaneHarness {
    pane: Entity<TerminalPane>,
}

struct TwoPaneHarness {
    first: Entity<TerminalPane>,
    second: Entity<TerminalPane>,
}

impl Render for PaneHarness {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        div().size_full().child(self.pane.clone())
    }
}

impl Render for TwoPaneHarness {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(self.first.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .h_full()
                    .child(self.second.clone()),
            )
    }
}

fn pane_with<'a>(
    content: &str,
    cx: &'a mut TestAppContext,
) -> (Entity<TerminalPane>, &'a mut VisualTestContext) {
    let pane = cx.new(|pane_cx| {
        TerminalPane::without_process(
            LocalShell::new("Test shell", "test-shell"),
            shellune_core::EventTag::new(shellune_core::SessionId::new(), 0),
            GridSize::new(40, 10),
            shellune_terminal::DEFAULT_FONT_SIZE,
            Locale::English,
            pane_cx,
        )
    });
    let harness_pane = pane.clone();
    let (_harness, cx) = cx.add_window_view(move |_window, _cx| PaneHarness { pane: harness_pane });

    if !content.is_empty() {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| pane.feed_for_test(content.as_bytes(), cx));
        });
    }
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    cx.update(|window, app| window.focus(pane.read(app).focus_handle()));
    cx.run_until_parked();
    (pane, cx)
}

/// A shell that started without its login environment says so, where it can be seen
/// (R14).
///
/// The note has to be on screen while the shell is still usable, and it has to be the
/// catalogue wording rather than the terminal crate English, or half the interface
/// would be translated and this line would not.
#[gpui::test]
async fn a_degraded_login_environment_is_visible_under_the_grid(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);

    // A shell that got everything it asked for claims nothing.
    assert!(
        cx.debug_bounds("terminal.environment").is_none(),
        "a healthy start must not show a degradation note"
    );

    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.set_environment_notes_for_test(
                vec![ShellEnvironmentNote::LoginEnvironmentUnavailable],
                cx,
            );
        });
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("terminal.environment").is_some(),
        "R14: the fallback is only honest if it can be read"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).environment_note_for_test()),
        Some(ENGLISH.terminal_environment_unavailable.to_owned()),
        "the bar must read the catalogue, not the English constant of the terminal crate"
    );

    // The same degradation in the other language: a translated surface, not a log
    // line that happens to be drawn.
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.set_locale(Locale::Chinese, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).environment_note_for_test()),
        Some(CHINESE.terminal_environment_unavailable.to_owned())
    );
}

/// A replaced locale and an environment that could not be read are both stated (R15).
#[gpui::test]
async fn a_replaced_locale_and_an_unreadable_environment_are_both_stated(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.set_environment_notes_for_test(
                vec![
                    ShellEnvironmentNote::LoginEnvironmentUnreadable,
                    ShellEnvironmentNote::LocaleReplaced,
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();

    let note = cx
        .update(|_window, app| pane.read(app).environment_note_for_test())
        .expect("both degradations were reported");
    assert!(
        note.contains(ENGLISH.terminal_environment_unreadable),
        "{note}"
    );
    assert!(
        note.contains(ENGLISH.terminal_environment_locale_replaced),
        "a replaced locale must not be swallowed by the other note: {note}"
    );
    assert!(cx.debug_bounds("terminal.environment").is_some());
}

#[gpui::test]
async fn the_terminal_shows_the_rows_its_shell_wrote(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("hello\r\nworld", cx);
    let viewport = cx.update(|window, _app| window.viewport_size());

    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()[0..2].to_vec()),
        vec!["hello".to_owned(), "world".to_owned()]
    );
    // One drawn row per grid row, and the grid is measured from the element.
    let grid = cx.update(|_window, app| pane.read(app).grid());
    assert!(grid.rows > 0 && grid.cols > 0);
    assert_fits_inside(
        bounds_of(cx, "terminal.viewport"),
        viewport,
        "terminal grid",
    );
    assert_fits_inside(bounds_of(cx, "terminal.row.0"), viewport, "first grid row");
    assert_fits_inside(bounds_of(cx, "terminal.bar"), viewport, "terminal bar");
    assert!(
        cx.debug_bounds("terminal.row.99").is_none(),
        "the pane must not draw rows the grid does not have"
    );
}

#[gpui::test]
async fn typing_reaches_the_shell_through_the_input_path(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);

    // Text arrives the way an input method commits it, not as key presses.
    cx.simulate_input("echo hi");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"echo hi".to_vec(),
        "typed text must reach the shell"
    );

    cx.simulate_keystrokes("enter");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"echo hi\r".to_vec(),
        "Return must send the carriage return a shell expects"
    );

    cx.simulate_keystrokes("ctrl-c");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"echo hi\r\x03".to_vec(),
        "Ctrl+C must interrupt, not be swallowed as a shortcut"
    );

    cx.simulate_keystrokes("up");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"echo hi\r\x03\x1b[A".to_vec(),
        "an arrow key must send the cursor sequence"
    );
}

fn bare_pane(name: &str, cx: &mut gpui::Context<TerminalPane>) -> TerminalPane {
    TerminalPane::without_process(
        LocalShell::new(name, "test-shell"),
        shellune_core::EventTag::new(shellune_core::SessionId::new(), 0),
        GridSize::new(40, 10),
        shellune_terminal::DEFAULT_FONT_SIZE,
        Locale::English,
        cx,
    )
}

fn two_panes(
    cx: &mut TestAppContext,
) -> (
    Entity<TerminalPane>,
    Entity<TerminalPane>,
    &mut VisualTestContext,
) {
    let first = cx.new(|pane_cx| bare_pane("Left shell", pane_cx));
    let second = cx.new(|pane_cx| bare_pane("Right shell", pane_cx));
    let first_pane = first.clone();
    let second_pane = second.clone();
    let (_harness, cx) = cx.add_window_view(move |_window, _cx| TwoPaneHarness {
        first: first_pane,
        second: second_pane,
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    (first, second, cx)
}

fn osc_bytes(payload: &str) -> Vec<u8> {
    let mut bytes = b"\x1b]".to_vec();
    bytes.extend_from_slice(payload.as_bytes());
    bytes.push(0x07);
    bytes
}

#[gpui::test]
async fn a_committed_line_is_written_without_a_paste_confirmation(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    cx.update(|window, app| {
        use gpui::EntityInputHandler;
        pane.update(app, |pane, cx| {
            pane.replace_text_in_range(None, "echo hi\n", window, cx);
        });
    });
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"echo hi\r".to_vec(),
        "an input-method commit that ends a line must reach the program"
    );
    assert!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().is_none()),
        "committing a line must not wait for a paste confirmation"
    );
}

#[gpui::test]
async fn a_multiline_paste_on_the_alternate_screen_shows_its_target_before_it_is_sent(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("", cx);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.feed_for_test(b"\x1b[?1049h", cx));
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| pane.read(app).alternate_for_test()),
        "the pane is on the alternate screen"
    );

    let paste = "echo one\necho two\n";
    let before = cx.update(|_window, app| pane.read(app).sent().to_vec());
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.stage_paste(paste, cx));
    });
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        before,
        "staging a paste on the alternate screen writes nothing"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_target().map(str::to_owned)),
        Some("Test shell".to_owned())
    );
    // The confirmation is the window's dialog now, not a card inside the pane: a pane
    // on its own draws none of it, and what it owes is the pending answer itself. The
    // dialog that draws this text is covered where the window draws it.
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        Some(paste.to_owned()),
        "the paste waits in the pane until it is answered"
    );

    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("ctrl-l");
    cx.simulate_keystrokes("ctrl-c");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        {
            let mut bytes = before.clone();
            bytes.extend_from_slice(&[0x1b, 0x0c, 0x03]);
            bytes
        },
        "Escape, Ctrl+L and Ctrl+C stay program input and do not confirm the paste"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        Some(paste.to_owned())
    );

    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.cancel_paste(cx));
    });
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        {
            let mut bytes = before;
            bytes.extend_from_slice(&[0x1b, 0x0c, 0x03]);
            bytes
        },
        "cancelling on the alternate screen writes nothing"
    );
    assert!(cx.update(|_window, app| pane.read(app).pending_paste_text().is_none()));
}

#[gpui::test]
async fn cancelling_a_multiline_paste_writes_nothing_and_program_keys_still_arrive(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("", cx);
    let paste = "echo one\necho two\n";
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.stage_paste(paste, cx));
    });
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        Vec::<u8>::new(),
        "a multi-line paste must not be written before it is confirmed"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_target().map(str::to_owned)),
        Some("Test shell".to_owned()),
        "the confirmation names the session that would receive the paste"
    );
    // A pane alone draws no dialog; the window draws it for the pane that asks.
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        Some(paste.to_owned())
    );

    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("ctrl-l");
    cx.simulate_keystrokes("ctrl-c");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        vec![0x1b, 0x0c, 0x03],
        "Escape, Ctrl+L and Ctrl+C stay program input while a paste is waiting"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        Some(paste.to_owned()),
        "those keys must not confirm or cancel the paste"
    );

    let before_cancel = cx.update(|_window, app| pane.read(app).sent().to_vec());
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.cancel_paste(cx));
    });
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        before_cancel,
        "cancelling a paste writes nothing"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        None
    );
}

#[gpui::test]
async fn confirming_a_multiline_paste_writes_only_the_active_pane(cx: &mut TestAppContext) {
    let (left, right, cx) = two_panes(cx);
    let paste = "echo left\necho more\n";
    cx.update(|_window, app| {
        left.update(app, |pane, cx| pane.stage_paste(paste, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| left.read(app).pending_paste_target().map(str::to_owned)),
        Some("Left shell".to_owned())
    );
    assert_eq!(
        cx.update(|_window, app| right.read(app).sent().to_vec()),
        Vec::<u8>::new()
    );
    assert!(cx.update(|_window, app| right.read(app).pending_paste_target().is_none()));

    cx.update(|_window, app| {
        left.update(app, |pane, cx| {
            pane.confirm_paste(cx);
        });
    });
    let expected = encode_paste(paste, TermMode::empty());
    assert_eq!(
        cx.update(|_window, app| left.read(app).sent().to_vec()),
        expected,
        "confirming writes the paste into the pane that asked"
    );
    assert_eq!(
        cx.update(|_window, app| right.read(app).sent().to_vec()),
        Vec::<u8>::new(),
        "the other session receives nothing"
    );
    assert!(cx.update(|_window, app| left.read(app).pending_paste_text().is_none()));

    let other = "echo right\necho rest\n";
    cx.update(|_window, app| {
        right.update(app, |pane, cx| pane.stage_paste(other, cx));
    });
    cx.run_until_parked();
    let left_sent = cx.update(|_window, app| left.read(app).sent().to_vec());
    cx.update(|_window, app| {
        right.update(app, |pane, cx| {
            pane.confirm_paste(cx);
        });
    });
    assert_eq!(
        cx.update(|_window, app| right.read(app).sent().to_vec()),
        encode_paste(other, TermMode::empty())
    );
    assert_eq!(
        cx.update(|_window, app| left.read(app).sent().to_vec()),
        left_sent,
        "confirming the other pane does not write into the first session"
    );
}

#[gpui::test]
async fn the_paste_confirmation_is_a_dialog_over_the_terminal_and_writes_nothing_until_answered(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("paste-dialog");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let (view, cx) = snippet_view(cx, &dir);
    let session = live_remote(cx, &view);
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(session).cloned())
        .expect("the session has a terminal");
    let paste = "echo one\necho two\n";
    let before = snippet_sent(cx, &view, session);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.stage_paste(paste, cx));
    });
    cx.run_until_parked();

    assert_eq!(
        snippet_sent(cx, &view, session),
        before,
        "nothing is written before the dialog is answered"
    );
    let viewport = cx.update(|window, _app| window.viewport_size());
    let dialog = bounds_of(cx, "terminal.paste");
    assert_fits_inside(dialog, viewport, "the paste dialog");
    // Centred over the window, not a card in the corner of the pane (R17).
    let centre = dialog.origin.y + dialog.size.height / 2.0;
    assert!(
        (centre - viewport.height / 2.0).abs() < viewport.height * 0.2,
        "the dialog is not centred over the window: {dialog:?} in {viewport:?}"
    );
    assert!(
        cx.debug_bounds("terminal.paste.target").is_some()
            && cx.debug_bounds("terminal.paste.lines").is_some()
            && cx.debug_bounds("terminal.paste.preview").is_some(),
        "the dialog names the target, the line count and the whole preview"
    );

    // The terminal behind the dialog receives no keys while it is up: the words are
    // spent on the dialog, so a program cannot be typed into through it (R17).
    cx.simulate_keystrokes("ctrl-l");
    cx.simulate_keystrokes("ctrl-c");
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, session),
        before,
        "the terminal behind the dialog takes no input"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        Some(paste.to_owned()),
        "those keys neither confirm nor cancel the paste"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::English,
        "Ctrl+L is bound to switching language, but no shortcut runs behind the dialog"
    );

    // Escape drops it, writes nothing and gives the keyboard back to the terminal.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, session),
        before,
        "cancelling the dialog writes nothing"
    );
    assert!(
        !cx.update(|_w, app| view.read(app).paste_prompt_open_for_test(app)),
        "the dialog leaves with the answer"
    );
    assert!(
        cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "the keyboard goes back to the terminal that asked"
    );

    // Confirming writes into the session the dialog named.
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.stage_paste(paste, cx));
    });
    cx.run_until_parked();
    click(cx, "terminal.paste.confirm");
    assert_eq!(
        snippet_sent(cx, &view, session),
        encode_paste(paste, TermMode::empty()),
        "confirming writes the paste into the terminal that asked for it"
    );
    assert!(
        !cx.update(|_w, app| view.read(app).paste_prompt_open_for_test(app))
            && cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "confirming closes the dialog and gives the keyboard back"
    );
}

#[gpui::test]
async fn answering_a_paste_over_the_manager_gives_the_keyboard_back_to_it(cx: &mut TestAppContext) {
    let dir = TempDir::new("paste-manager-focus");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let (view, cx) = snippet_view(cx, &dir);
    let session = live_remote(cx, &view);
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(session).cloned())
        .expect("the session has a terminal");
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(false, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.update(|window, app| view.read(app).snippet_focus_for_test().is_focused(window)),
        "opening the manager already gives it the keyboard"
    );
    let before = snippet_sent(cx, &view, session);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.stage_paste("echo one\necho two\n", cx));
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_w, app| view.read(app).paste_prompt_open_for_test(app)),
        "the multi-line paste asks first"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        !cx.update(|_w, app| view.read(app).paste_prompt_open_for_test(app)),
        "escape answers the dialog rather than the manager behind it"
    );
    assert_eq!(
        cx.update(|_w, app| view.read(app).tool_for_test()),
        Some(crate::app::view::Tool::Snippets),
        "the manager stays open behind the answer"
    );
    assert_eq!(
        snippet_sent(cx, &view, session),
        before,
        "cancelling the dialog writes nothing"
    );
    assert!(
        cx.update(|window, app| view.read(app).snippet_focus_for_test().is_focused(window)),
        "the manager that was still on screen keeps the keyboard"
    );
    assert!(
        !cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "the terminal behind the manager does not take input the person cannot see"
    );
}

#[gpui::test]
async fn the_paste_dialog_writes_only_into_the_session_it_names(cx: &mut TestAppContext) {
    let dir = TempDir::new("paste-target");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let (view, cx) = snippet_view(cx, &dir);
    let first = live_remote(cx, &view);
    let second = live_remote_second(cx, &view, first);
    let first_pane = cx
        .update(|_window, app| view.read(app).pane_of(first).cloned())
        .expect("the first session has a terminal");
    let paste = "echo first\necho only\n";
    let first_before = snippet_sent(cx, &view, first);
    let second_before = snippet_sent(cx, &view, second);
    cx.update(|_window, app| {
        first_pane.update(app, |pane, cx| pane.stage_paste(paste, cx));
    });
    cx.run_until_parked();

    // The session on screen is the second one; the dialog still belongs to the pane
    // that asked, so an answer cannot be written into the tab that happens to be up.
    assert!(
        cx.debug_bounds("terminal.paste.target").is_some(),
        "the waiting pane's dialog is found even when another tab is on screen"
    );
    click(cx, "terminal.paste.confirm");
    assert_eq!(
        snippet_sent(cx, &view, first),
        [first_before.clone(), encode_paste(paste, TermMode::empty())].concat(),
        "confirming writes into the session the dialog named"
    );
    assert_eq!(
        snippet_sent(cx, &view, second),
        second_before,
        "the session on screen receives nothing"
    );
}

#[gpui::test]
async fn integration_markers_become_blocks_and_the_alternate_screen_stays_classic(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("", cx);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.set_presentation(TerminalMode::Integrated, cx);
            let mut bytes = Vec::new();
            bytes.extend(osc_bytes("133;A"));
            bytes.extend(b"$ ");
            bytes.extend(osc_bytes("133;B"));
            bytes.extend(b"echo hello");
            bytes.extend(osc_bytes("133;C"));
            bytes.extend(b"\r\nhello\r\n");
            pane.feed_for_test(&bytes, cx);
        });
    });
    cx.run_until_parked();
    let running = cx.update(|_window, app| pane.read(app).command_blocks());
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].command, "echo hello");
    assert_eq!(running[0].output, "hello");
    assert_eq!(running[0].status, BlockStatus::Running);
    assert!(!running[0].status.is_success());
    assert!(running[0].duration.is_none());
    assert!(cx.debug_bounds("terminal.blocks").is_some());

    std::thread::sleep(std::time::Duration::from_millis(20));
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.feed_for_test(&osc_bytes("133;D;0"), cx)
        });
    });
    cx.run_until_parked();
    let finished = cx.update(|_window, app| pane.read(app).command_blocks());
    assert_eq!(finished[0].status, BlockStatus::Exited(0));
    assert!(finished[0].status.is_success());
    assert_eq!(finished[0].output, "hello");
    let duration = finished[0]
        .duration
        .expect("a finished command has a duration");
    assert!(duration.as_millis() >= 1, "{duration:?}");

    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            let mut bytes = Vec::new();
            bytes.extend(osc_bytes("133;B"));
            bytes.extend(b"false");
            bytes.extend(osc_bytes("133;C"));
            bytes.extend(b"\r\nnope\r\n");
            bytes.extend(osc_bytes("133;D"));
            pane.feed_for_test(&bytes, cx);
        });
    });
    let blocks = cx.update(|_window, app| pane.read(app).command_blocks());
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].status, BlockStatus::Exited(0));
    assert_eq!(blocks[1].command, "false");
    assert_eq!(blocks[1].status, BlockStatus::Unknown);
    assert!(
        !blocks[1].status.is_success(),
        "a command-end marker with no exit code is not success"
    );

    let primary = cx.update(|_window, app| pane.read(app).visible_text());
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.feed_for_test(b"\x1b[?1049h\x1b[Hinside", cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| pane.read(app).alternate_for_test()),
        "the full-screen program switched to the alternate screen"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()[0].clone()),
        "inside",
        "the alternate screen is the classic grid"
    );
    assert!(
        !cx.update(|_window, app| pane.read(app).shows_blocks_for_test()),
        "command blocks stay hidden on the alternate screen"
    );

    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.feed_for_test(b"\x1b[?1049l", cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()),
        primary,
        "leaving the alternate screen restores the previous grid"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).command_blocks()),
        blocks,
        "the full-screen program does not rewrite recorded commands"
    );
    assert!(cx.update(|_window, app| pane.read(app).shows_blocks_for_test()));

    let kept = cx.update(|_window, app| pane.read(app).visible_text());
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.set_presentation(TerminalMode::Classic, cx)
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()),
        kept,
        "turning command blocks off does not rewrite the grid"
    );
    assert!(!cx.update(|_window, app| pane.read(app).shows_blocks_for_test()));
}

// ---------------------------------------------------------------------------
// Terminal search (R1)
// ---------------------------------------------------------------------------

/// Forty lines, every tenth of which holds the word the find bar tests search for.
fn lines_with_needles() -> String {
    needle_lines(40)
}

/// `count` lines, every tenth of which holds the word the find bar tests search for.
fn needle_lines(count: usize) -> String {
    (0..count)
        .map(|line| {
            if line % 10 == 0 {
                format!("needle line {line}")
            } else {
                format!("line {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn search_state_of(
    pane: &Entity<TerminalPane>,
    cx: &mut VisualTestContext,
) -> crate::app::terminal::SearchBarState {
    cx.update(|_window, app| pane.read(app).search_state())
}

#[gpui::test]
async fn the_find_bar_counts_the_places_and_steps_both_ways(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with(&lines_with_needles(), cx);
    click(cx, "terminal.search");
    assert_eq!(
        search_state_of(&pane, cx),
        crate::app::terminal::SearchBarState {
            open: true,
            focused: true,
            query: String::new(),
            case_sensitive: false,
            regex: false,
            note: SearchNote::Silent,
            blocks: 0,
        },
        "opening the bar starts with an empty query and no count to claim"
    );

    cx.simulate_input("needle");
    cx.run_until_parked();
    let found = search_state_of(&pane, cx);
    assert_eq!(
        found.note,
        SearchNote::Position {
            position: 1,
            total: 4
        },
        "every line with the word is a place, and the first one is current: {found:?}"
    );
    assert!(
        cx.debug_bounds("terminal.search.position").is_some(),
        "the bar draws which place of how many it is at"
    );

    // Both the keyboard and the buttons step through the same list.
    cx.simulate_keystrokes("enter");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 2,
            total: 4
        },
        "Enter steps on"
    );
    click(cx, "terminal.search.next");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 3,
            total: 4
        }
    );
    click(cx, "terminal.search.previous");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 2,
            total: 4
        },
        "the button steps back"
    );
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 4
        },
        "Shift+Enter steps back"
    );
    cx.simulate_keystrokes("shift-enter");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 4,
            total: 4
        },
        "stepping back from the first place wraps to the last"
    );
}

#[gpui::test]
async fn closing_the_find_bar_restores_the_scroll_position_and_the_keyboard(
    cx: &mut TestAppContext,
) {
    // Long enough that the whole history cannot fit on one screen: the pane grows to
    // the test window, so a short transcript would leave nothing to scroll through.
    let (pane, cx) = pane_with(&needle_lines(400), cx);
    let center = bounds_of(cx, "terminal.viewport").center();
    cx.simulate_event(ScrollWheelEvent {
        position: center,
        delta: ScrollDelta::Lines(point(0.0, 3.0)),
        modifiers: Modifiers::default(),
        touch_phase: Default::default(),
    });
    cx.run_until_parked();
    let before = cx.update(|_window, app| pane.read(app).display_offset());
    assert!(before > 0, "the wheel moved the viewport back into history");

    click(cx, "terminal.search");
    cx.simulate_input("needle");
    cx.run_until_parked();
    assert_ne!(
        cx.update(|_window, app| pane.read(app).display_offset()),
        before,
        "locating a match moves the viewport to it"
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).display_offset()),
        before,
        "closing the bar puts the reader back on the line they were reading"
    );
    assert!(!search_state_of(&pane, cx).open);
    assert!(
        cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "the keyboard is back on the terminal"
    );
    cx.simulate_input("echo");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"echo".to_vec(),
        "typing reaches the shell again instead of the closed bar"
    );
}

/// History long enough to park the viewport, with three matches far enough apart
/// that a one-line shift cannot make the next match look like the current one.
fn parked_history() -> String {
    let mut lines = Vec::with_capacity(120);
    lines.push("mark-alpha".to_owned());
    for index in 0..40 {
        lines.push(format!("filler {index}"));
    }
    lines.push("mark-beta".to_owned());
    for index in 40..80 {
        lines.push(format!("filler {index}"));
    }
    lines.push("mark-gamma".to_owned());
    for index in 80..120 {
        lines.push(format!("filler {index}"));
    }
    lines.join("\r\n")
}

#[gpui::test]
async fn new_output_while_searching_parked_history_keeps_the_place_and_the_hit(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with(&parked_history(), cx);
    let center = bounds_of(cx, "terminal.viewport").center();
    cx.simulate_event(ScrollWheelEvent {
        position: center,
        delta: ScrollDelta::Lines(point(0.0, 3.0)),
        modifiers: Modifiers::default(),
        touch_phase: Default::default(),
    });
    cx.run_until_parked();
    let parked = cx.update(|_window, app| pane.read(app).display_offset());
    assert!(
        parked > 0,
        "the viewport is parked in history, not on the live row"
    );
    let reading = cx.update(|_window, app| pane.read(app).visible_text());

    click(cx, "terminal.search");
    cx.simulate_input("mark-");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 3
        },
        "the first of the three marks is current"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text().first().cloned()),
        Some("mark-alpha".to_owned()),
        "locating the first mark puts that line at the top"
    );

    // One new row while the bar is open and the viewport is not pinned to the
    // bottom. The grid keeps the same rows on screen by growing display_offset;
    // the saved offset and the saved match line must grow with it.
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.feed_for_test(b"\r\nfresh line\r\n", cx)
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| pane.read(app).display_offset()) > parked,
        "the parked viewport's offset grew with the new row"
    );
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 3
        },
        "the count stays on the mark that was current, not the next one the shift made look later"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text().first().cloned()),
        Some("mark-alpha".to_owned()),
        "recounting leaves the reader on the same mark"
    );

    cx.simulate_keystrokes("enter");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 2,
            total: 3
        },
        "the next place is the next mark, not the one after the shifted index"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text().first().cloned()),
        Some("mark-beta".to_owned()),
        "the next place is mark-beta, not mark-gamma"
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()),
        reading,
        "closing puts back the rows that were on screen before the search"
    );
}

/// `without_process` keeps 1000 lines. This is longer than that, so the oldest
/// rows have already rotated out before the search opens.
fn full_scrollback_lines(rows: usize) -> String {
    let mut lines = Vec::with_capacity(rows + 1030);
    for index in 0..rows + 1030 {
        lines.push(format!("id-{index:05}"));
    }
    lines.join("\r\n")
}

#[gpui::test]
async fn a_full_scrollback_keeps_the_place_and_the_hit_when_output_arrives(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("", cx);
    let rows = cx.update(|_window, app| pane.read(app).grid().rows);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.feed_for_test(full_scrollback_lines(rows).as_bytes(), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).history_len()),
        1000,
        "the test pane's scrollback is full, not merely long"
    );

    let center = bounds_of(cx, "terminal.viewport").center();
    cx.simulate_event(ScrollWheelEvent {
        position: center,
        delta: ScrollDelta::Lines(point(0.0, 2000.0)),
        modifiers: Modifiers::default(),
        touch_phase: Default::default(),
    });
    cx.run_until_parked();
    cx.simulate_event(ScrollWheelEvent {
        position: center,
        delta: ScrollDelta::Lines(point(0.0, -400.0)),
        modifiers: Modifiers::default(),
        touch_phase: Default::default(),
    });
    cx.run_until_parked();
    let parked = cx.update(|_window, app| pane.read(app).display_offset());
    assert!(
        parked > 0 && parked < 1000,
        "the viewport is in the middle of a full history, not at an end, got {parked}"
    );
    let reading = cx.update(|_window, app| pane.read(app).visible_text());
    let top = reading[0].clone();
    assert!(
        top.starts_with("id-"),
        "the parked row is one of the filled lines, got {top:?}"
    );

    click(cx, "terminal.search");
    cx.simulate_input(&top);
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        },
        "the parked row is the only match"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text().first().cloned()),
        Some(top.clone()),
        "locating the parked row leaves it at the top"
    );

    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.feed_for_test(b"\r\nfresh-line\r\n", cx)
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| pane.read(app).display_offset()) > parked,
        "the parked viewport's offset grew with the new row"
    );
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        },
        "the current hit is still the parked row, not a later line"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text().first().cloned()),
        Some(top),
        "recounting leaves the same row on screen"
    );

    cx.simulate_event(ScrollWheelEvent {
        position: center,
        delta: ScrollDelta::Lines(point(0.0, -30.0)),
        modifiers: Modifiers::default(),
        touch_phase: Default::default(),
    });
    cx.run_until_parked();
    assert_ne!(
        cx.update(|_window, app| pane.read(app).visible_text()),
        reading,
        "the wheel moved off the parked rows while the bar was open"
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()),
        reading,
        "closing puts back the rows that were on screen before the search"
    );
}

#[gpui::test]
async fn the_case_and_expression_switches_narrow_the_same_query(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("needle\r\nNeedle", cx);
    click(cx, "terminal.search");
    cx.simulate_input("needle");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 2
        },
        "literal text folds case by default"
    );
    assert!(
        cx.debug_bounds("terminal.search.keys").is_some(),
        "the bar names the keys that set the search up while it has the keyboard"
    );

    // Both switches are reachable from the keyboard, which is what lets a search be
    // set up without a pointer (R1, R4).
    cx.simulate_keystrokes("alt-c");
    cx.run_until_parked();
    let exact = search_state_of(&pane, cx);
    assert!(exact.case_sensitive, "Alt+C turns the switch on: {exact:?}");
    assert_eq!(
        exact.note,
        SearchNote::Position {
            position: 1,
            total: 1
        },
        "only the line spelled the same way matches"
    );
    cx.simulate_keystrokes("alt-c");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 2
        },
        "Alt+C turns it off again"
    );

    for _ in 0.."needle".len() {
        cx.simulate_keystrokes("backspace");
    }
    cx.simulate_input("^N.edle$");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Empty,
        "literal text is not read as an expression"
    );

    cx.simulate_keystrokes("alt-r");
    cx.run_until_parked();
    let expression = search_state_of(&pane, cx);
    assert!(
        expression.regex,
        "Alt+R turns the switch on: {expression:?}"
    );
    assert_eq!(
        expression.note,
        SearchNote::Position {
            position: 1,
            total: 2
        },
        "the same text read as an expression finds both lines: {expression:?}"
    );
    assert!(
        cx.debug_bounds("terminal.search.position").is_some(),
        "a working expression shows where it is, not an error"
    );
}

#[gpui::test]
async fn a_paste_while_the_find_bar_is_open_edits_the_query(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("needle\r\nother", cx);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
        "needle\r\nother".to_owned(),
    ));
    click(cx, "terminal.search");
    cx.dispatch_action(crate::app::actions::TerminalPaste);
    cx.run_until_parked();

    assert_eq!(
        search_state_of(&pane, cx).query,
        "needle other",
        "pasted text edits the query, with its line breaks flattened"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        Vec::<u8>::new(),
        "a copy-pasted command must not reach the shell behind the bar"
    );
}

#[gpui::test]
async fn an_empty_query_an_empty_result_and_a_broken_expression_stay_apart(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("needle\r\nother", cx);
    let before = cx.update(|_window, app| pane.read(app).visible_text());
    click(cx, "terminal.search");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Silent,
        "an empty query says nothing about matches"
    );

    cx.simulate_input("absent");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Empty,
        "a query nothing matches is an empty result"
    );
    assert!(
        cx.debug_bounds("terminal.search.empty").is_some(),
        "the empty result is drawn"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()),
        before,
        "a search that matches nothing neither moves the viewport nor rewrites the grid"
    );

    for _ in 0.."absent".len() {
        cx.simulate_keystrokes("backspace");
    }
    cx.simulate_keystrokes("alt-r");
    cx.run_until_parked();
    cx.simulate_input("need(le");
    cx.run_until_parked();
    let broken = search_state_of(&pane, cx);
    assert!(
        matches!(broken.note, SearchNote::Invalid(_)),
        "an unclosed group is reported instead of being counted: {broken:?}"
    );
    assert!(
        cx.debug_bounds("terminal.search.error").is_some(),
        "the reason is drawn"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).visible_text()),
        before,
        "an unusable expression leaves the terminal as it was"
    );
}

#[gpui::test]
async fn the_find_bar_narrows_the_command_blocks_with_the_same_switches(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.set_presentation(TerminalMode::Integrated, cx);
            let mut bytes = Vec::new();
            bytes.extend(osc_bytes("133;B"));
            bytes.extend(b"echo Hello");
            bytes.extend(osc_bytes("133;C"));
            bytes.extend(b"\r\nHello output\r\n");
            bytes.extend(osc_bytes("133;D;0"));
            pane.feed_for_test(&bytes, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).blocks,
        1,
        "the recorded command is listed"
    );
    assert!(cx.debug_bounds("terminal.block.1").is_some());

    click(cx, "terminal.search");
    cx.simulate_input("hello");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).blocks,
        1,
        "the block list folds case the way the scrollback does"
    );

    cx.simulate_keystrokes("alt-c");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).blocks,
        0,
        "a case-sensitive query drops the block too: {:?}",
        search_state_of(&pane, cx)
    );

    // The buttons still do the same thing for a pointer, and the switches keep their
    // state between them.
    click(cx, "terminal.search.case");
    cx.simulate_keystrokes("alt-r");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).blocks,
        1,
        "a valid expression keeps the block"
    );

    click(cx, "terminal.search.close");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).blocks,
        1,
        "closing the bar shows every block again"
    );
    assert!(!search_state_of(&pane, cx).open);
}

#[gpui::test]
async fn a_full_screen_program_keeps_the_find_bar_shut_and_its_keys(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    // On an ordinary screen the same action opens the bar, so the check below is
    // about the full-screen program and not about a listener that never ran.
    cx.dispatch_action(crate::app::actions::TerminalSearch);
    cx.run_until_parked();
    assert!(
        search_state_of(&pane, cx).open,
        "the search action opens the bar"
    );
    cx.simulate_keystrokes("escape");
    assert!(!search_state_of(&pane, cx).open);

    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.feed_for_test(b"\x1b[?1049h\x1b[Hinside", cx)
        });
    });
    cx.run_until_parked();
    assert!(cx.update(|_window, app| pane.read(app).alternate_for_test()));

    cx.dispatch_action(crate::app::actions::TerminalSearch);
    cx.run_until_parked();
    assert!(
        !search_state_of(&pane, cx).open,
        "a full-screen program's screen is not this pane's history to search"
    );

    // The button on the bar refuses as well, and the keys stay with the program.
    click(cx, "terminal.search");
    assert!(!search_state_of(&pane, cx).open);
    cx.simulate_input("q");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"q".to_vec(),
        "the typed key reaches the full-screen program instead of a bar"
    );
}

#[gpui::test]
async fn a_full_screen_program_hides_the_bar_and_gives_it_back(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("needle\r\nother", cx);
    click(cx, "terminal.search");
    cx.simulate_input("needle");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        }
    );

    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.feed_for_test(b"\x1b[?1049h\x1b[Hinside", cx)
        });
    });
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Empty,
        "the full-screen grid is not the history that matched"
    );
    cx.simulate_input("q");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"q".to_vec(),
        "typing belongs to the full-screen program, not to the bar behind it"
    );

    cx.update(|_window, app| pane.update(app, |pane, cx| pane.feed_for_test(b"\x1b[?1049l", cx)));
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        },
        "leaving the full-screen program finds the earlier history again"
    );
}

#[gpui::test]
async fn the_two_switches_take_their_keys_only_while_the_bar_holds_them(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("needle", cx);
    click(cx, "terminal.search");
    cx.simulate_input("needle");
    cx.simulate_keystrokes("alt-c");
    let held = search_state_of(&pane, cx);
    assert!(
        held.case_sensitive,
        "Alt+C is the bar's while it has the keys"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        Vec::<u8>::new(),
        "a bar key never reaches the shell"
    );

    cx.simulate_keystrokes("escape");
    assert!(!search_state_of(&pane, cx).open, "Escape closed the bar");

    // With the bar closed the same combinations belong to the program again: these
    // are the bytes a shell reads as Alt+C and Alt+R, not a shortcut.
    cx.simulate_keystrokes("alt-c");
    cx.simulate_keystrokes("alt-r");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"\x1bc\x1br".to_vec(),
        "closing the bar hands Alt+C and Alt+R back to the terminal"
    );
    assert!(
        !search_state_of(&pane, cx).open,
        "a program key does not reopen the bar"
    );
}

#[gpui::test]
async fn the_bar_says_which_of_the_two_has_the_keys(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("needle\r\nneedle", cx);
    click(cx, "terminal.search");
    cx.simulate_input("needle");
    cx.run_until_parked();

    let held = search_state_of(&pane, cx);
    assert!(held.focused, "the bar holds the keyboard: {held:?}");
    assert!(
        cx.debug_bounds("terminal.search.keyboard.bar").is_some(),
        "the query field is drawn as the one with the keyboard"
    );
    assert!(
        cx.debug_bounds("terminal.search.keys.terminal").is_none(),
        "nothing claims the terminal has the keys while the bar does"
    );
    assert_eq!(
        held.note,
        SearchNote::Position {
            position: 1,
            total: 2
        }
    );

    // The line naming the keys and the switches themselves stay inside the smallest
    // window, where the bar has the least room.
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    let small = cx.update(|window, _app| window.viewport_size());
    assert_fits_inside(
        bounds_of(cx, "terminal.search.keys"),
        small,
        "the find bar's key hint",
    );
    assert_fits_inside(
        bounds_of(cx, "terminal.search.case"),
        small,
        "the case switch",
    );
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    // Clicking the grid hands the keys back to the shell and leaves the bar open: the
    // reader has to be able to tell the two states apart, so the field turns neutral
    // and the bar says where the keys go.
    click(cx, "terminal.viewport");
    let back = search_state_of(&pane, cx);
    assert!(!back.focused, "the terminal has the keys again: {back:?}");
    assert_eq!(back.query, "needle", "the query is still on the bar");
    assert!(
        cx.debug_bounds("terminal.search.keyboard.terminal")
            .is_some(),
        "the query field is drawn as the one without the keyboard"
    );
    assert!(
        cx.debug_bounds("terminal.search.keys.terminal").is_some(),
        "the bar says the keys go to the terminal"
    );
    assert_eq!(
        back.note,
        SearchNote::Position {
            position: 1,
            total: 2
        },
        "the count keeps showing while the terminal has the keys"
    );
    // And the keys are the shell's now, exactly as the line above says: the bar's
    // switches do not change behind the reader's back.
    cx.simulate_keystrokes("alt-c");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"\x1bc".to_vec(),
        "with the terminal holding the keys, Alt+C reaches the program"
    );
    assert!(
        !search_state_of(&pane, cx).case_sensitive,
        "the case switch did not turn over while the keys were the shell's"
    );
}

#[gpui::test]
async fn a_resize_recounts_the_matches_on_the_new_wrap(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    // The narrow width comes first, so the text is written at the width it is wrapped
    // at and the match below straddles a break rather than having to find one.
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    let narrow = cx.update(|_window, app| pane.read(app).grid());
    assert!(narrow.cols > 8, "the narrow grid still has columns");
    let mut line = "x".repeat(narrow.cols - 3);
    line.push_str("needle");
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.feed_for_test(line.as_bytes(), cx));
    });
    cx.run_until_parked();

    click(cx, "terminal.search");
    cx.simulate_input("needle");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Empty,
        "at this width the word is split by the wrap, so nothing matches: {:?}",
        search_state_of(&pane, cx)
    );

    // A wider grid joins the two rows again. The count and the places it steps
    // through follow the new layout instead of the numbers from before it.
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let wide = cx.update(|_window, app| pane.read(app).grid());
    assert!(wide.cols > narrow.cols, "the window really got wider");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        },
        "the match the narrower grid split is counted again: {:?}",
        search_state_of(&pane, cx)
    );
    assert!(
        cx.debug_bounds("terminal.search.position").is_some(),
        "the reopened count is drawn"
    );
    // Stepping still lands on a line of the new layout, and there is one place, so it
    // wraps to itself.
    cx.simulate_keystrokes("enter");
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        },
        "stepping through the recounted places works after the resize"
    );
}

/// The measured parts of the find bar, plus the bar itself.
///
/// Each is a box the walkthrough would have to reach with a pointer: the query and the
/// count are read, the switches and the two arrows are pressed, and the last one closes
/// the bar (R1, R4).
const FIND_BAR_PARTS: [&str; 8] = [
    "terminal.search.query",
    "terminal.search.keys",
    "terminal.search.previous",
    "terminal.search.next",
    "terminal.search.case",
    "terminal.search.regex",
    "terminal.search.close",
    "terminal.search.bar",
];

/// The find bar at the size the walkthrough measures it: the minimum window in the
/// largest text, with the session drawn inside the window rather than the pane alone.
///
/// The bar hangs over the grid at the bottom of the pane, so every part of it has to be
/// inside the window and inside the panel it belongs to. A part drawn past either edge
/// is drawn past the window where neither the pointer nor a scroll can bring it back
/// (R4); this test reads the boxes instead of the drawing, so a squeezed part still has
/// to be inside them.
#[gpui::test]
async fn the_find_bar_stays_inside_the_window_at_two_hundred_percent(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the session has a terminal");
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    cx.dispatch_action(CycleTextSize);
    cx.dispatch_action(CycleTextSize);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|window, _app| window.viewport_size()),
        minimum_window(),
        "the bar is not being measured at the minimum window"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest
    );

    click(cx, "terminal.search");
    cx.run_until_parked();
    let query = bounds_of(cx, "terminal.search.query");
    // The find bar uses the terminal metrics, which stay at the regular size.
    let placeholder = px("Search this terminal".chars().count() as f32
        * (Metrics::new(Zoom::Regular).caption_size() / px(1.0))
        * shellune_terminal::MONOSPACE_ADVANCE_RATIO);
    assert!(
        query.size.width + px(1.0) >= placeholder,
        "the empty search field cuts the placeholder through a word: {query:?} needs {placeholder:?}"
    );
    cx.simulate_input("ready");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        },
        "the bar has something to count at this size"
    );

    // Both languages: the words on the bar are the widest thing in it, and the count and
    // the switches have to share whatever the Chinese labels leave them.
    for locale in [Locale::English, Locale::Chinese] {
        if locale == Locale::Chinese {
            cx.dispatch_action(ToggleLanguage);
            cx.run_until_parked();
            assert_eq!(
                cx.update(|_window, app| view.read(app).workspace().locale()),
                Locale::Chinese
            );
        }
        let viewport = cx.update(|window, _app| window.viewport_size());
        let panel = bounds_of(cx, "content");
        assert!(
            cx.debug_bounds("terminal.search.position").is_some(),
            "the count is drawn in {locale:?}"
        );
        for selector in FIND_BAR_PARTS {
            let bounds = bounds_of(cx, selector);
            assert_fits_inside(bounds, viewport, selector);
            assert_inside_box(bounds, panel, selector);
        }
    }

    // And it is still a bar, not just a drawing of one: its close button answers at this
    // size and hands the keys back to the terminal.
    click(cx, "terminal.search.close");
    cx.run_until_parked();
    let closed = search_state_of(&pane, cx);
    assert!(!closed.open, "the close button closed the bar: {closed:?}");
    assert!(!closed.focused, "the terminal has the keys again");
}

/// The bar in one pane of a sideways split of the minimum window.
///
/// At that size the sidebar is an icon strip and the two panes share the 720 px: each
/// pane is 354 px wide with the 12 px divider between them, so the bar inside one is
/// 336 px wide. The line naming the keys is wider than that on its own, so it takes a
/// line of its own and wraps there instead of running past the pane's right edge, where
/// the rest of it — including the key that closes the bar — would be unreadable (R4).
/// The size is the measured pane box, not a guess; the split test below measures the
/// same bar inside a real split.
#[gpui::test]
async fn the_find_bar_narrows_with_the_pane_instead_of_leaving_it(cx: &mut TestAppContext) {
    let (_pane, cx) = pane_with("needle line\r\nother", cx);
    cx.simulate_resize(size(px(354.0), px(392.0)));
    cx.run_until_parked();
    click(cx, "terminal.search");
    cx.simulate_input("needle");
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    let bar = bounds_of(cx, "terminal.search.bar");
    let query = bounds_of(cx, "terminal.search.query");
    let hint = bounds_of(cx, "terminal.search.keys");
    assert!(
        hint.size.width <= bar.size.width + px(0.5),
        "the key hint is drawn past the bar: {hint:?} vs {bar:?}"
    );
    assert!(
        hint.size.height > query.size.height,
        "the hint kept one line where there is no room for one: {hint:?} vs {query:?}"
    );
    for selector in FIND_BAR_PARTS {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    assert!(
        cx.debug_bounds("terminal.search.position").is_some(),
        "the count is drawn in the narrow pane too"
    );
}

/// The reason under a broken expression, in a pane as narrow as one half of the
/// minimum window.
///
/// A sideways split of 720 px gives each pane 354 px (the divider takes 12). The
/// engine's reason for `a{2,1}` — the start of a repetition is past its end — is a
/// sentence wider than that pane. The count, the empty result and that reason are
/// the three notes beside the query; a note that cannot shrink is drawn past the
/// pane's right edge, and the pane clips it, so the end of the reason cannot be
/// read (R4). This measures the same width as
/// [the_find_bar_narrows_with_the_pane_instead_of_leaving_it], which is the width a
/// real split pane has. The boxes are the test window's `debug_bounds`, not a
/// Windows, macOS or Linux walkthrough.
#[gpui::test]
async fn a_broken_expression_reason_wraps_inside_the_narrow_pane(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("needle\r\nother", cx);
    cx.simulate_resize(size(px(354.0), px(392.0)));
    cx.run_until_parked();
    click(cx, "terminal.search");
    cx.simulate_keystrokes("alt-r");
    cx.simulate_input("a{2,1}");
    cx.run_until_parked();

    let note = search_state_of(&pane, cx).note;
    let SearchNote::Invalid(reason) = &note else {
        panic!(
            "a repetition whose start is past its end is an invalid expression, not a count: {note:?}"
        );
    };
    assert!(
        reason.contains("the start must be <= the end"),
        "the bar is showing the engine's reason, whose tail is what a narrow pane used to clip: {reason}"
    );
    let before = cx.update(|_window, app| pane.read(app).visible_text());
    assert!(
        before.iter().any(|line| line.contains("needle")),
        "a broken expression must not clear the grid: {before:?}"
    );

    let viewport = cx.update(|window, _app| window.viewport_size());
    let bar = bounds_of(cx, "terminal.search.bar");
    let query = bounds_of(cx, "terminal.search.query");
    let error = bounds_of(cx, "terminal.search.error");
    assert_fits_inside(bar, viewport, "terminal.search.bar");
    assert_fits_inside(error, viewport, "terminal.search.error");
    assert_inside_box(error, bar, "the invalid-expression reason");
    assert!(
        error.size.width <= bar.size.width + px(0.5),
        "the reason is drawn past the bar: {error:?} vs {bar:?}"
    );
    assert!(
        error.size.height > query.size.height,
        "the reason stayed on one line and ran past the pane: {error:?} vs {query:?}"
    );
    for selector in [
        "terminal.search.close",
        "terminal.search.case",
        "terminal.search.regex",
        "terminal.search.previous",
        "terminal.search.next",
    ] {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
        assert_inside_box(bounds_of(cx, selector), bar, selector);
    }
}

/// The same bar inside a real sideways split of the minimum window.
///
/// [the_find_bar_narrows_with_the_pane_instead_of_leaving_it] measures the width a split
/// pane has; this one takes it from the window, so the two sizes cannot drift apart: the
/// bar and every part of it stays inside the pane it belongs to and inside the window.
#[gpui::test]
async fn a_find_bar_in_a_split_pane_stays_inside_its_pane(cx: &mut TestAppContext) {
    let Some((view, cx, _left, right)) = split_tab(cx) else {
        // No shell to start: nothing is measured here, and a skip is not a pass.
        return;
    };
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(right).cloned())
        .expect("the split has two terminals");
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    feed_session(cx, &view, right, b"needle line\r\n");

    cx.dispatch_action(crate::app::actions::TerminalSearch);
    cx.run_until_parked();
    assert!(
        search_state_of(&pane, cx).open,
        "the keys opened the bar in the pane they were typed in"
    );
    cx.simulate_input("needle");
    cx.run_until_parked();
    assert_eq!(
        search_state_of(&pane, cx).note,
        SearchNote::Position {
            position: 1,
            total: 1
        }
    );

    let viewport = cx.update(|window, _app| window.viewport_size());
    let frame = bounds_of(cx, "pane.active");
    assert!(
        frame.size.width < px(400.0),
        "the split really shares the window: {frame:?}"
    );
    for selector in FIND_BAR_PARTS {
        let bounds = bounds_of(cx, selector);
        assert_fits_inside(bounds, viewport, selector);
        assert_inside_box(bounds, frame, selector);
    }
}

#[gpui::test]
async fn chinese_text_is_sent_as_utf8_and_composition_is_not_sent_early(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);

    cx.simulate_input("你好");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        "你好".as_bytes().to_vec(),
        "committed Chinese text must reach the shell as UTF-8"
    );

    // A composition that has not been committed yet stays out of the byte stream:
    // it is shown next to the caret until the input method commits it.
    let sent_before = cx.update(|_window, app| pane.read(app).sent().len());
    let marked = cx.update(|window, app| {
        use gpui::EntityInputHandler;
        pane.update(app, |pane, cx| {
            pane.replace_and_mark_text_in_range(None, "nihao", None, window, cx);
            pane.marked_text_range(window, cx)
        })
    });
    assert!(marked.is_some(), "the composition must be tracked");
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().len()),
        sent_before,
        "an uncommitted composition must not be sent to the shell"
    );
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("terminal.composition").is_some(),
        "the composition must be visible while it is being typed"
    );

    // Committing replaces the composition with its final text and sends it once.
    cx.update(|window, app| {
        use gpui::EntityInputHandler;
        pane.update(app, |pane, cx| {
            pane.replace_text_in_range(None, "你好", window, cx);
        });
    });
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        "你好你好".as_bytes().to_vec(),
        "the committed text is sent and not repeated"
    );
}

#[gpui::test]
async fn a_mouse_selection_is_copied_when_it_is_released(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("hello world", cx);
    let viewport = bounds_of(cx, "terminal.viewport");
    let cell = cx.update(|_window, app| pane.read(app).cell_size());
    let origin = viewport.origin;
    let y = origin.y + px(cell.height * 0.5);
    let first = point(origin.x + px(cell.width * 0.5), y);
    let fifth = point(origin.x + px(cell.width * 4.5), y);

    cx.simulate_mouse_down(first, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(fifth, Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(fifth, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| pane.read(app).last_copy().map(str::to_owned)),
        Some("hello".to_owned()),
        "selecting with the mouse must put the text on the clipboard"
    );
    let clipboard =
        cx.update(|_window, app| app.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(clipboard, Some("hello".to_owned()));
}

#[gpui::test]
async fn a_click_in_the_terminal_leaves_the_clipboard_untouched(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("hello world", cx);

    // What the user copied elsewhere, before clicking into the terminal to paste it.
    cx.update(|_window, app| {
        app.write_to_clipboard(ClipboardItem::new_string("copied elsewhere".to_owned()));
    });

    let viewport = bounds_of(cx, "terminal.viewport");
    let cell = cx.update(|_window, app| pane.read(app).cell_size());
    // Pressing and releasing on the same cell: moving the caret, not selecting text.
    let target = point(
        viewport.origin.x + px(cell.width * 0.5),
        viewport.origin.y + px(cell.height * 0.5),
    );
    cx.simulate_mouse_down(target, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| app.read_from_clipboard().and_then(|item| item.text())),
        Some("copied elsewhere".to_owned()),
        "a click must not replace the clipboard with the one cell under the pointer"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).last_copy().map(str::to_owned)),
        None,
        "a click selects nothing, so there is nothing to copy"
    );
}

#[gpui::test]
async fn the_wheel_scrolls_back_through_the_history(cx: &mut TestAppContext) {
    let content = (0..60)
        .map(|line| format!("line {line}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let (pane, cx) = pane_with(&content, cx);

    assert_eq!(
        cx.update(|_window, app| pane.read(app).display_offset()),
        0,
        "a new terminal starts at the live output"
    );
    let viewport = bounds_of(cx, "terminal.viewport");
    let center = viewport.center();
    cx.simulate_event(ScrollWheelEvent {
        position: center,
        delta: ScrollDelta::Lines(point(0.0, 3.0)),
        modifiers: Modifiers::default(),
        touch_phase: Default::default(),
    });
    cx.run_until_parked();

    assert!(
        cx.update(|_window, app| pane.read(app).display_offset()) > 0,
        "the wheel must move the viewport back into the history"
    );
}

#[gpui::test]
async fn the_font_size_controls_change_the_grid(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("abc", cx);
    let before = cx.update(|_window, app| {
        let pane = pane.read(app);
        (pane.font_size(), pane.grid().cols)
    });

    let bounds = bounds_of(cx, "terminal.font.increase");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    let after = cx.update(|_window, app| {
        let pane = pane.read(app);
        (pane.font_size(), pane.grid().cols)
    });
    assert!(after.0 > before.0, "the control must raise the font size");
    assert!(
        after.1 <= before.1,
        "a larger font must not fit more columns than before"
    );

    let bounds = bounds_of(cx, "terminal.font.decrease");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).font_size()),
        before.0,
        "the controls step back down again"
    );
}

#[gpui::test]
async fn a_shell_that_does_not_exist_shows_the_reason_and_keeps_no_session(
    cx: &mut TestAppContext,
) {
    let mut workspace = Workspace::new();
    workspace.set_shells(vec![LocalShell::new(
        "Missing shell",
        "shellune-no-such-shell-zz9",
    )]);
    workspace.select_shell(0);
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(view.pane().is_none(), "no terminal may be shown");
        let workspace = view.workspace();
        assert_eq!(
            workspace.state(Destination::LocalShells),
            SectionState::Failed
        );
        assert_eq!(workspace.item_count(Destination::LocalShells), 0);
        let reason = workspace
            .failure(Destination::LocalShells)
            .map(Failure::reason)
            .expect("the failure must say what went wrong");
        assert!(
            reason.contains("shellune-no-such-shell-zz9"),
            "the reason must name the shell that failed: {reason}"
        );
    });
    assert!(
        cx.debug_bounds("terminal.viewport").is_none(),
        "a failed launch must not draw a terminal"
    );
    assert!(
        cx.update(|_window, app| view.read(app).local_error_for_test())
            .is_some(),
        "the failure is said on the home page"
    );
}

#[gpui::test]
async fn the_picker_lists_the_shells_and_remembers_the_choice(cx: &mut TestAppContext) {
    let mut workspace = Workspace::new();
    workspace.set_shells(vec![
        LocalShell::new("First", "first-shell"),
        LocalShell::new("Second", "second-shell"),
    ]);
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    assert!(cx.debug_bounds("shell.choice.0").is_some());
    let bounds = bounds_of(cx, "shell.choice.1");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(workspace.shell_choice(), 1);
        assert_eq!(
            workspace.selected_shell().map(LocalShell::program),
            Some("second-shell")
        );
    });
}

#[gpui::test]
async fn a_custom_shell_path_starts_exactly_what_was_typed(cx: &mut TestAppContext) {
    let mut workspace = Workspace::new();
    workspace.set_shells(Vec::new());
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let bounds = bounds_of(cx, "shell.custom");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("shell.path").is_some(),
        "the path field must open"
    );

    cx.simulate_input("shellune-no-such-shell-qq1");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    cx.update(|_window, app| {
        let view = view.read(app);
        let workspace = view.workspace();
        assert!(
            view.pane().is_none(),
            "a wrong path must not produce a terminal"
        );
        let reason = workspace
            .failure(Destination::LocalShells)
            .map(Failure::reason)
            .unwrap_or_default();
        assert!(
            reason.contains("shellune-no-such-shell-qq1"),
            "the failure must name the program that was typed: {reason}"
        );
    });
}

#[gpui::test]
async fn a_custom_shell_that_starts_owns_the_keyboard(cx: &mut TestAppContext) {
    let shell = shellune_terminal::platform_default();
    if !shellune_terminal::program_exists(shell.program()) {
        return;
    }
    let mut workspace = Workspace::new();
    workspace.set_shells(Vec::new());
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let bounds = bounds_of(cx, "shell.custom");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("shell.path").is_some(),
        "the path field must open"
    );

    cx.simulate_input(shell.program());
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("the typed shell must start");
    assert!(
        cx.update(|_window, app| matches!(pane.read(app).state(), PaneState::Live)),
        "the shell named in the field must really be running"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).custom_shell_open()),
        "the field closes once its text became a session"
    );
    assert!(
        cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "the keyboard must belong to the shell, not to the field that started it"
    );

    // What the user types next is shell input, and Return reaches the shell instead
    // of starting the same program again.
    cx.simulate_input("echo typed");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"echo typed\r".to_vec(),
        "typing after the start must reach the shell"
    );
    assert_eq!(
        cx.update(|_window, app| { view.read(app).workspace().local_sessions().count() }),
        1,
        "a second Return must not start the shell again"
    );
    // The field emptied itself, so reopening it cannot offer the same path again. The
    // terminal now fills the page, so the field is reached through the header's menu.
    assert!(
        !cx.update(|_window, app| view.read(app).shell_menu_open()),
        "the menu closed once its path became a session"
    );
    let bounds = bounds_of(cx, "shell.menu");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    let bounds = bounds_of(cx, "shell.custom");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).shell_path_text(app)),
        "",
        "the field must not still hold the path that became a session"
    );
}

#[gpui::test]
async fn a_custom_shell_that_fails_keeps_its_text_while_another_terminal_is_open(
    cx: &mut TestAppContext,
) {
    // The case the field used to get wrong: a terminal is already on screen, so
    // "a pane exists" was true for a reason that had nothing to do with the path the
    // user just typed. The field closed and emptied itself over a start that failed,
    // and there was no way to correct the path.
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }

    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).pane().is_some()),
        "the first terminal must really be open for this case to mean anything"
    );

    // With a terminal on screen the shell choices live in the header's menu, and
    // the custom path opens as a dialog.
    let bounds = bounds_of(cx, "shell.menu");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    let bounds = bounds_of(cx, "shell.custom");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("shell.path").is_some(),
        "the path field must open"
    );

    cx.simulate_input("shellune-no-such-shell-qq3");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("shell.path").is_some(),
        "a start that failed must leave the field open so the path can be corrected"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).shell_path_text(app)),
        "shellune-no-such-shell-qq3",
        "the typed path must survive its own failure"
    );
    assert!(
        cx.update(|_window, app| view.read(app).custom_shell_open()),
        "the field stays open because nothing started from it"
    );

    // The dialog is modal; Escape closes it and gives the keyboard back.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).custom_shell_open()));

    // The failed launch is local. The shell that is already open, the navigation and
    // the command palette keep working.
    let open = cx
        .update(|_window, app| view.read(app).active_session())
        .expect("the first shell is still the current session");
    let sent = sent_after_typing(cx, &view, open, "after-failed-launch");
    assert!(
        sent.contains("after-failed-launch"),
        "the open shell still takes input: {sent:?}"
    );
    click(cx, "nav.hosts");
    assert_eq!(destination_of(&view, cx), Destination::Hosts);
    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(open)
    );
    go(cx, crate::app::actions::OpenPalette);
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
}

#[gpui::test]
async fn a_shell_picked_from_the_menu_opens_another_tab_and_closes_the_menu(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }
    assert!(
        cx.debug_bounds("shell.choice.0").is_some(),
        "the home page shows the shells as cards, not only behind a menu"
    );

    click(cx, "header.primary");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().local_sessions().count()),
        1,
        "the main button opened the first shell"
    );

    click(cx, "shell.menu");
    assert!(
        cx.debug_bounds("shell.choice.0").is_some(),
        "the menu lists the shells"
    );
    click(cx, "shell.choice.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().local_sessions().count()),
        2,
        "a shell picked from the menu opens in a tab of its own"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).shell_menu_open()),
        "picking a shell closes the menu"
    );
    assert!(cx.debug_bounds("session.tab.1").is_some());
}

#[gpui::test]
async fn a_real_shell_starts_from_the_header_action(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        // Nothing to start on a machine without a shell; the other tests cover the
        // failure that this produces.
        return;
    }

    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a terminal must be shown");
    let (live, session_state, grid) = cx.update(|_window, app| {
        let pane = pane.read(app);
        (
            matches!(pane.state(), PaneState::Live),
            view.read(app)
                .workspace()
                .local_sessions()
                .next()
                .map(|session| session.state().clone()),
            pane.grid(),
        )
    });
    assert!(live, "the real shell process must be running");
    assert_eq!(
        session_state,
        Some(SessionState::Connected),
        "a started shell must be listed as connected"
    );
    assert!(grid.rows > 0 && grid.cols > 0);
    assert_fits_inside(
        bounds_of(cx, "terminal.viewport"),
        cx.update(|window, _app| window.viewport_size()),
        "the real terminal grid",
    );

    // The shell's own output must reach the grid through the same path the window
    // uses: PTY -> terminal core -> the rows the view draws.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut screen = String::new();
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| {
                pane.pump_once(cx);
            });
        });
        screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
        if !screen.trim().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !screen.trim().is_empty(),
        "the real shell printed nothing into the terminal grid"
    );
    assert!(
        cx.debug_bounds("terminal.row.0").is_some(),
        "the grid rows of the running shell must be drawn"
    );
}

/// R8 in the path the product actually ships: the default local shell, not `cmd.exe`.
///
/// The interrupt tests in `tests/local_shell.rs` drive `cmd.exe`, which is not what a
/// local terminal starts on Windows. This test starts a terminal the way a user does,
/// types a program that keeps running, and presses Ctrl+C through the pane's own key
/// handler. What it asserts is the pane's half: the press becomes exactly one `0x03`
/// in the default shell's PTY while the program is still printing, and the session
/// survives it. Whether the console host turns that byte into an interrupt is not
/// something a test window can schedule, so it is measured against the same default
/// shell at the PTY level by
/// `tests/local_shell.rs::interrupt::the_default_shell_forwards_the_interrupt_to_the_program_it_started`
/// and in the running application in `docs/release/0.1.0-verification.md` 7.1.
#[gpui::test]
async fn a_ctrl_c_reaches_the_default_shell_that_is_running_a_command(cx: &mut TestAppContext) {
    let default_shell = shellune_terminal::platform_default();
    if !shellune_terminal::program_exists(default_shell.program()) {
        return;
    }
    #[cfg(windows)]
    assert!(
        default_shell
            .program()
            .to_ascii_lowercase()
            .contains("windowspowershell"),
        "this test must cover the shell a local terminal starts on Windows, got {}",
        default_shell.program()
    );

    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a terminal must be shown");

    // The shell that runs is the machine's default one, not the `cmd.exe` the PTY-level
    // interrupt tests use; that is the gap this test closes.
    let title = cx.update(|_window, app| {
        view.read(app)
            .workspace()
            .local_sessions()
            .next()
            .map(|session| session.title().to_owned())
            .unwrap_or_default()
    });
    assert!(
        title.contains(default_shell.name()),
        "the session has to be the default shell {}, not {title}",
        default_shell.name()
    );

    // The prompt has to be there before anything is typed, exactly as for a user.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut screen = String::new();
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| pane.pump_once(cx));
        });
        screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
        if !screen.trim().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !screen.trim().is_empty(),
        "the shell never printed a prompt"
    );

    // The program has to be really running before the key is pressed, or the press proves
    // nothing. On Windows a ping prints one reply per second and `TTL=` is the part of
    // that line no locale changes. Elsewhere ping takes other flags and often needs raw
    // socket rights, so a shell loop prints a marker instead; the marker is computed by
    // the shell, so the echoed command line itself never counts as a reply.
    #[cfg(windows)]
    let (command, marker) = ("ping -n 60 127.0.0.1", "TTL=");
    #[cfg(not(windows))]
    let (command, marker) = (
        "while :; do echo shellune-$((40+2)); sleep 0.5; done",
        "shellune-42",
    );
    cx.simulate_input(command);
    cx.simulate_keystrokes("enter");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut replies = 0;
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| pane.pump_once(cx));
        });
        screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
        replies = screen.matches(marker).count();
        if replies >= 2 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        replies >= 2,
        "the command must be printing before Ctrl+C means anything: {replies} replies on \
         screen. The screen held:\n{screen}"
    );

    let before = cx.update(|_window, app| pane.read(app).sent().len());
    cx.simulate_keystrokes("ctrl-c");
    cx.run_until_parked();
    let sent = cx.update(|_window, app| pane.read(app).sent()[before..].to_vec());
    assert_eq!(
        sent,
        vec![0x03],
        "Ctrl+C must send exactly one interrupt byte to the default shell, got {sent:?}"
    );
    let live = cx.update(|_window, app| matches!(pane.read(app).state(), PaneState::Live));
    assert!(live, "an interrupt must end the command, not the session");
}

/// Escape inside a terminal belongs to the program running in it.
///
/// Found in the T11 walkthrough: Vim started in the local terminal could not leave
/// insert mode, because the application's notice shortcut took Escape before the
/// pane's key handler saw it. The grid declares its own key context now and the
/// shortcut is bound to the shell context without it.
#[gpui::test]
async fn escape_reaches_the_program_running_in_the_terminal(cx: &mut TestAppContext) {
    let default_shell = shellune_terminal::platform_default();
    if !shellune_terminal::program_exists(default_shell.program()) {
        return;
    }
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    // The real bindings have to be installed or the window has no Escape binding at all
    // and the key falls through to the pane no matter where the shortcut is scoped.
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a terminal must be shown");
    assert!(
        cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "the terminal owns the keyboard when it starts"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let sent = cx.update(|_window, app| pane.read(app).sent().to_vec());
    assert_eq!(
        sent,
        vec![0x1b],
        "Escape must reach the shell as a key, not be taken by the application"
    );
}

/// The notice shortcut must not be reachable while a terminal has the keyboard.
///
/// The behavioural test above runs through the pane's own key handler, which a test
/// window drives even when a binding would win in the real window; this one pins the
/// binding itself, which is what made Vim in the shipped application impossible to
/// leave (measured: with the plain shell context Vim stayed in insert mode and the
/// quit command was typed into the buffer; with this predicate Vim exited).
#[test]
fn the_notice_shortcut_steps_aside_for_a_terminal() {
    let bindings = key_bindings(Platform::Windows);
    let escape = bindings
        .iter()
        .find(|binding| {
            binding
                .keystrokes()
                .iter()
                .any(|keystroke| keystroke.key() == "escape")
        })
        .expect("the notice shortcut is bound to Escape");
    let predicate = format!("{:?}", escape.predicate());
    assert!(
        predicate.contains("Not") && predicate.contains(crate::app::TERMINAL_KEY_CONTEXT),
        "Escape has to be bound outside the terminal context, got {predicate}"
    );
}

/// Ctrl+L inside a terminal belongs to the program running in it.
///
/// The same defect class as Escape, found in delivery review: the language toggle was
/// bound to the window-wide context, so a shell never received `0x0c` and nothing in
/// it could clear the screen. Measured before the fix: the pane's byte log stayed
/// empty and the interface language flipped instead.
#[gpui::test]
async fn ctrl_l_reaches_the_program_running_in_the_terminal(cx: &mut TestAppContext) {
    let default_shell = shellune_terminal::platform_default();
    if !shellune_terminal::program_exists(default_shell.program()) {
        return;
    }
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    // The real bindings have to be installed, or the window has no Ctrl+L binding at
    // all and the key reaches the pane no matter where the shortcut is scoped.
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a terminal must be shown");
    assert!(
        cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "the terminal owns the keyboard when it starts"
    );

    cx.simulate_keystrokes("ctrl-l");
    cx.run_until_parked();
    let sent = cx.update(|_window, app| pane.read(app).sent().to_vec());
    assert_eq!(
        sent,
        vec![0x0c],
        "Ctrl+L must reach the shell as clear-screen, not be taken by the language shortcut"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::English,
        "the language must not change when Ctrl+L is pressed inside a terminal"
    );

    // The settings page is the in-terminal path to the same intent: it has to keep
    // working, or the shortcut above would have made the language unreachable.
    go(cx, crate::app::actions::GoToSettings);
    click(cx, "settings.item.language");
    click(cx, "settings.language.1");
    go(cx, GoToLocalShells);
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese,
        "the language control must switch the interface language"
    );
    let after_click = cx.update(|_window, app| pane.read(app).sent().to_vec());
    assert_eq!(
        after_click,
        vec![0x0c],
        "clicking the language control must not type into the shell"
    );

    // And the key keeps reaching the shell after the switch. Clicking a control takes
    // the keyboard out of the pane - the status bar is outside the terminal, as it is
    // for the text-size control - so the keyboard is given back the way a user does it.
    let viewport = bounds_of(cx, "terminal.viewport");
    let viewport_center = viewport.center();
    cx.simulate_click(
        point(viewport_center.x, viewport_center.y),
        Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(
        cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "clicking the terminal gives it the keyboard back"
    );
    cx.simulate_keystrokes("ctrl-l");
    cx.run_until_parked();
    let sent = cx.update(|_window, app| pane.read(app).sent().to_vec());
    assert_eq!(
        sent,
        vec![0x0c, 0x0c],
        "Ctrl+L must keep reaching the shell whatever the interface language is"
    );
}

/// The language shortcut must not be reachable while a terminal has the keyboard.
///
/// The behavioural test above runs through the pane's own key handler, which a test
/// window drives even when a binding would win in the real window; this one pins the
/// binding itself, the way the Escape test pins the notice shortcut.
#[test]
fn the_language_shortcut_steps_aside_for_a_terminal() {
    let bindings = key_bindings(Platform::Windows);
    let language = bindings
        .iter()
        .find(|binding| {
            binding
                .keystrokes()
                .iter()
                .any(|keystroke| keystroke.key() == "l")
        })
        .expect("the language shortcut is bound to L");
    let predicate = format!("{:?}", language.predicate());
    assert!(
        predicate.contains("Not") && predicate.contains(crate::app::TERMINAL_KEY_CONTEXT),
        "Ctrl+L has to be bound outside the terminal context, got {predicate}"
    );
}

#[gpui::test]
async fn a_real_shell_that_exits_stops_being_listed_as_connected(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }

    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a terminal must be shown");

    // Leave the shell the way a user does.
    cx.simulate_input("exit");
    cx.simulate_keystrokes("enter");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut exited = false;
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| {
                pane.pump_once(cx);
            });
        });
        exited =
            cx.update(|_window, app| matches!(pane.read(app).state(), PaneState::Exited { .. }));
        if exited {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(exited, "the shell exit was never noticed by the pane");

    let state = cx.update(|_window, app| {
        view.read(app)
            .workspace()
            .local_sessions()
            .next()
            .map(|session| session.state().clone())
    });
    assert!(
        matches!(
            state,
            Some(SessionState::Disconnected(_)) | Some(SessionState::Closed)
        ),
        "an exited shell must not stay listed as connected, got {state:?}"
    );
    // The last screen stays readable instead of the terminal disappearing.
    assert!(
        cx.debug_bounds("terminal.viewport").is_some(),
        "the terminal keeps showing what the shell left behind"
    );
    // Why it ended and what can be done about it are both on screen: a reason without
    // the action it allows leaves the user to guess (R12).
    assert!(
        cx.debug_bounds("terminal.failure").is_some(),
        "the reason the shell stopped must be stated above its last screen"
    );
    assert!(
        cx.debug_bounds("terminal.failure.next").is_some(),
        "the failure must state the next step, not only the reason"
    );
}

// ---------------------------------------------------------------------------
// Host management (R3)
// ---------------------------------------------------------------------------

/// A directory that removes itself when the test ends.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "shellune-app-{}-{}-{tag}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        let _ = std::fs::remove_file(&path);
        std::fs::create_dir_all(&path).expect("the test directory can be created");
        Self { path }
    }

    fn path(&self) -> PathBuf {
        self.path.clone()
    }

    fn store(&self) -> HostStore {
        HostStore::at(&self.path)
    }

    fn file(&self) -> PathBuf {
        self.path.join("hosts.json")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Mounts the shell on the hosts destination with an explicit store.
fn hosts_view(
    cx: &mut TestAppContext,
    store: HostStore,
) -> (Entity<ShelluneView>, &mut VisualTestContext) {
    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_store(cx, workspace, store));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    (view, cx)
}

/// Opens the connection form of the saved host at `index`, as the palette does.
fn open_saved_host(cx: &mut VisualTestContext, view: &Entity<ShelluneView>, index: usize) {
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            let host = view.book().hosts()[index].clone();
            view.open_remote(&host, cx);
        });
    });
    cx.run_until_parked();
}

/// Dispatches a keyboard action and lets the window settle.
fn go(cx: &mut VisualTestContext, action: impl gpui::Action) {
    cx.dispatch_action(action);
    cx.run_until_parked();
}

/// Clicks the centre of a rendered element.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = bounds_of(cx, selector);
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
}

fn right_click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = bounds_of(cx, selector);
    let center = bounds.center();
    cx.simulate_mouse_down(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.run_until_parked();
}

/// Moves the pointer onto the centre of a rendered element, the way a person does
/// before clicking it.
fn hover(cx: &mut VisualTestContext, selector: &'static str) {
    let center = bounds_of(cx, selector).center();
    cx.simulate_mouse_move(point(center.x, center.y), None, Modifiers::default());
    cx.run_until_parked();
}

/// Clicks a form field and types into it, the way a user does.
fn type_into(cx: &mut VisualTestContext, selector: &'static str, text: &str) {
    click(cx, selector);
    cx.simulate_input(text);
    cx.run_until_parked();
}

/// Empties a field from its end, which is where the caret starts.
fn clear_field(cx: &mut VisualTestContext, selector: &'static str, characters: usize) {
    click(cx, selector);
    for _ in 0..characters {
        cx.simulate_keystrokes("backspace");
    }
    cx.run_until_parked();
}

fn stored_host(
    name: &str,
    address: &str,
    port: u16,
    user: &str,
    group: Option<&str>,
) -> HostConfig {
    let mut host = HostConfig::new(name, address, port, user).expect("a valid test host");
    host.set_group(group);
    host
}

/// Writes a book with the given hosts, as a previous run would have left it.
fn seed(store: &HostStore, hosts: Vec<HostConfig>) -> HostBook {
    // A fresh book, not a load: some tests seed over a file that is deliberately
    // damaged, and the point of the seed is what the next run must read.
    let mut book = HostBook::new();
    for host in hosts {
        book.apply(&HostEdit::create(host))
            .expect("the host is inserted");
    }
    store.save(&book).expect("the book saves");
    book
}

#[gpui::test]
async fn a_new_host_is_created_through_the_form_and_reaches_the_file(cx: &mut TestAppContext) {
    let dir = TempDir::new("create");
    let (view, cx) = hosts_view(cx, dir.store());

    assert!(
        cx.debug_bounds("hosts.editor").is_none(),
        "no form before the action"
    );
    assert!(
        cx.debug_bounds("hosts.empty").is_some(),
        "an empty store explains itself"
    );

    click(cx, "hosts.new");
    assert!(
        cx.debug_bounds("hosts.editor").is_some(),
        "the primary action opens the form"
    );

    type_into(cx, "hosts.field.name", "build");
    type_into(cx, "hosts.field.address", "10.0.0.7");
    clear_field(cx, "hosts.field.port", 2);
    cx.simulate_input("2222");
    cx.run_until_parked();
    type_into(cx, "hosts.field.user", "deploy");
    type_into(cx, "hosts.field.group", "Prod");

    click(cx, "hosts.save");

    cx.update(|_window, app| {
        let view = view.read(app);
        let book = view.book();
        assert_eq!(book.hosts().len(), 1, "one host was created");
        let host = &book.hosts()[0];
        assert_eq!(host.name(), "build");
        assert_eq!(host.address(), "10.0.0.7");
        assert_eq!(host.port().get(), 2222);
        assert_eq!(host.user(), "deploy");
        assert_eq!(host.group(), Some("Prod"));

        let workspace = view.workspace();
        assert_eq!(
            workspace.state(Destination::Hosts),
            SectionState::Populated,
            "the saved host is a real result"
        );
        assert_eq!(workspace.item_count(Destination::Hosts), 1);
        // Saving a host is not connecting to it.
        assert_eq!(workspace.item_count(Destination::Sessions), 0);
        assert_eq!(workspace.item_count(Destination::LocalShells), 0);
        assert!(view.pane().is_none(), "no terminal was started");
    });

    // The debug bounds of this GPUI version accumulate across frames, so a closed
    // form is asserted through the state and through what the list now shows.
    assert!(
        cx.update(|_window, app| !view.read(app).hosts_state().is_editing()),
        "a stored host closes the form"
    );
    assert!(
        cx.debug_bounds("hosts.list.1").is_some(),
        "the list now holds the one saved host"
    );
    assert!(
        cx.debug_bounds("hosts.row.0").is_some(),
        "the new host is listed"
    );
    assert!(
        cx.debug_bounds("hosts.message").is_some(),
        "the shell says what happened"
    );
    assert!(dir.file().is_file(), "the record reached the disk");
    assert!(
        std::fs::read_to_string(dir.file())
            .expect("the file is readable")
            .contains("10.0.0.7"),
        "the file holds the address that was typed"
    );
}

#[gpui::test]
async fn the_key_path_of_a_host_is_visible_and_editable_in_the_form(cx: &mut TestAppContext) {
    // An imported IdentityFile is part of the record, so it has to be readable and
    // changeable afterwards; only the path is stored, never the key itself.
    let dir = TempDir::new("identity");
    let store = dir.store();
    let mut imported = stored_host("web", "10.0.0.5", 22, "deploy", None);
    imported.set_identity_file(Some("C:/keys/web_ed25519"));
    let book = seed(&store, vec![imported]);
    let id = book.hosts()[0].id();

    let (view, cx) = hosts_view(cx, HostStore::at(dir.path()));
    click(cx, "hosts.edit.0");
    assert!(
        cx.debug_bounds("hosts.field.identity").is_some(),
        "the key path has a field of its own"
    );
    cx.update(|_window, app| {
        let draft = view
            .read(app)
            .hosts_state()
            .draft()
            .expect("a form is open");
        assert_eq!(
            draft.text(crate::app::hosts::HostField::IdentityFile),
            "C:/keys/web_ed25519",
            "the path that was imported is shown in the form"
        );
    });

    clear_field(cx, "hosts.field.identity", "C:/keys/web_ed25519".len());
    type_into(cx, "hosts.field.identity", "C:/keys/work_ed25519.pem");
    click(cx, "hosts.save");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(view.book().hosts().len(), 1, "an edit must not add a host");
        assert_eq!(
            view.book()
                .host(id)
                .expect("the same record")
                .identity_file(),
            Some("C:/keys/work_ed25519.pem"),
            "the edited path is what the record holds"
        );
    });
    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    assert_eq!(
        restarted.host(id).expect("the same record").identity_file(),
        Some("C:/keys/work_ed25519.pem"),
        "the edited path survives a restart"
    );

    // An emptied field clears the reference; it does not save an empty path that
    // would look like a key location.
    click(cx, "hosts.edit.0");
    clear_field(cx, "hosts.field.identity", "C:/keys/work_ed25519.pem".len());
    click(cx, "hosts.save");
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app)
                .book()
                .host(id)
                .expect("the same record")
                .identity_file(),
            None,
            "a blank field removes the key path"
        );
    });
}

#[gpui::test]
async fn an_invalid_port_is_refused_and_nothing_is_written(cx: &mut TestAppContext) {
    let dir = TempDir::new("port");
    let (view, cx) = hosts_view(cx, dir.store());

    click(cx, "hosts.new");
    type_into(cx, "hosts.field.name", "web");
    type_into(cx, "hosts.field.address", "10.0.0.5");
    type_into(cx, "hosts.field.user", "deploy");
    clear_field(cx, "hosts.field.port", 2);
    cx.simulate_input("0");
    cx.run_until_parked();

    click(cx, "hosts.save");

    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(
            view.book().hosts().is_empty(),
            "a refused port must not create a host"
        );
        let draft = view
            .hosts_state()
            .draft()
            .expect("the form stays open so the field can be corrected");
        assert_eq!(draft.text(crate::app::hosts::HostField::Port), "0");
        let message = view
            .hosts_state()
            .message()
            .expect("the refusal is explained");
        assert!(message.failed(), "a refusal is a failure");
        assert!(
            message.text().contains("1-65535"),
            "the reason names the range: {}",
            message.text()
        );
    });
    assert!(
        !dir.file().exists(),
        "a refused input must not create the host file"
    );
    assert!(
        cx.debug_bounds("hosts.message").is_some(),
        "the reason is on screen"
    );
}

#[gpui::test]
async fn a_saved_host_is_listed_again_in_a_new_window_and_its_edit_is_persisted(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("edit");
    let store = dir.store();
    let book = seed(
        &store,
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let id = book.hosts()[0].id();

    // A new window on the same directory is what a restart looks like to the shell.
    let (view, cx) = hosts_view(cx, HostStore::at(dir.path()));
    assert!(
        cx.debug_bounds("hosts.row.0").is_some(),
        "the host saved by the previous run is listed"
    );

    click(cx, "hosts.edit.0");
    assert!(cx.debug_bounds("hosts.editor").is_some());
    cx.update(|_window, app| {
        let draft = view
            .read(app)
            .hosts_state()
            .draft()
            .expect("a form is open");
        assert_eq!(
            draft.text(crate::app::hosts::HostField::Address),
            "10.0.0.5"
        );
        assert_eq!(draft.text(crate::app::hosts::HostField::User), "deploy");
        assert_eq!(draft.text(crate::app::hosts::HostField::Port), "22");
    });

    clear_field(cx, "hosts.field.address", "10.0.0.5".len());
    cx.simulate_input("web.example.org");
    cx.run_until_parked();
    clear_field(cx, "hosts.field.user", "deploy".len());
    cx.simulate_input("ops");
    cx.run_until_parked();

    click(cx, "hosts.save");

    cx.update(|_window, app| {
        let view = view.read(app);
        let book = view.book();
        assert_eq!(book.hosts().len(), 1, "an edit must not add a host");
        let host = book.host(id).expect("the same record");
        assert_eq!(host.target(), "ops@web.example.org:22");
        assert_eq!(view.workspace().item_count(Destination::Sessions), 0);
    });

    let restarted = HostStore::at(dir.path()).load().expect("the file loads");
    assert_eq!(restarted.hosts().len(), 1);
    assert_eq!(
        restarted.host(id).expect("the same record").target(),
        "ops@web.example.org:22",
        "the edited address and user are what the next run reads"
    );
}

#[gpui::test]
async fn the_three_views_and_the_search_read_the_one_saved_book(cx: &mut TestAppContext) {
    let dir = TempDir::new("views");
    let store = dir.store();
    let book = seed(
        &store,
        vec![
            stored_host("web", "10.0.0.5", 22, "deploy", Some("Prod")),
            stored_host("api", "api.example.org", 2222, "svc", Some("Prod")),
            stored_host("lab", "lab.internal", 22, "root", None),
        ],
    );
    let web = book.hosts()[0].id();

    let (view, cx) = hosts_view(cx, store);
    assert!(cx.debug_bounds("hosts.row.0").is_some());
    assert!(cx.debug_bounds("hosts.row.1").is_some());
    assert!(cx.debug_bounds("hosts.row.2").is_some());
    assert!(
        cx.debug_bounds("hosts.row.3").is_none(),
        "three hosts must not render four rows"
    );

    click(cx, "hosts.view.groups");
    assert!(
        cx.debug_bounds("hosts.group.0").is_some(),
        "the named group has a heading"
    );
    assert!(
        cx.debug_bounds("hosts.group.1").is_some(),
        "the hosts without a group get their own heading"
    );

    click(cx, "hosts.view.recent");
    assert!(
        cx.debug_bounds("hosts.recent.empty").is_some(),
        "saving a host is not connecting to it, so nothing is recent yet"
    );
    assert!(cx.debug_bounds("hosts.recent.0").is_none());

    // The remote-connection step records a real connection through this path.
    let recorded = cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.record_host_connection(web, 1_700_000_000, cx)
        })
    });
    assert!(recorded, "the recency record reached the file");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("hosts.recent.0").is_some(),
        "a host that really connected is listed in the recent view"
    );
    let reloaded = HostStore::at(dir.path()).load().expect("the file loads");
    assert_eq!(reloaded.recent_hosts().len(), 1);
    assert_eq!(reloaded.recent_hosts()[0].host.name(), "web");

    click(cx, "hosts.view.all");
    type_into(cx, "hosts.search", "api");
    assert!(
        cx.debug_bounds("hosts.list.1").is_some(),
        "the search narrows the list to the one matching host"
    );

    type_into(cx, "hosts.search", "zzz");
    assert!(
        cx.debug_bounds("hosts.nomatch").is_some(),
        "an empty search result says so instead of looking empty"
    );
    assert!(
        cx.debug_bounds("hosts.list.0").is_some(),
        "no row is left after a search that matches nothing"
    );
}

#[gpui::test]
async fn a_damaged_host_file_is_visible_and_only_a_real_read_clears_it(cx: &mut TestAppContext) {
    let dir = TempDir::new("damaged");
    std::fs::write(dir.file(), "{ this is not a host book }").expect("the damaged file is written");

    let (view, cx) = hosts_view(cx, dir.store());
    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(workspace.state(Destination::Hosts), SectionState::Failed);
        assert_eq!(workspace.item_count(Destination::Hosts), 0);
        let reason = workspace
            .failure(Destination::Hosts)
            .map(Failure::reason)
            .unwrap_or_default()
            .to_owned();
        assert!(
            reason.contains("hosts.json"),
            "the reason names the file: {reason}"
        );
    });
    assert!(
        cx.debug_bounds("state.card").is_some(),
        "the damage is visible where the hosts would be"
    );

    click(cx, "hosts.retry");
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).workspace().state(Destination::Hosts),
            SectionState::Failed,
            "retrying a file that is still damaged keeps the failure"
        );
    });

    // A readable file is what clears it — not a retry on its own.
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    click(cx, "hosts.retry");
    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(workspace.state(Destination::Hosts), SectionState::Populated);
        assert_eq!(workspace.item_count(Destination::Hosts), 1);
    });
    assert!(cx.debug_bounds("hosts.row.0").is_some());
}

#[gpui::test]
async fn a_write_that_the_platform_refuses_keeps_the_list_and_reports_the_reason(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("write-fail");
    let store = dir.store();
    seed(
        &store,
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );

    let (view, cx) = hosts_view(cx, store);

    // The data directory becomes a file: the next save cannot even create the
    // directory it would write into.
    std::fs::remove_dir_all(dir.path()).expect("the directory is removed");
    std::fs::write(dir.path(), "not a directory").expect("the path becomes a file");

    click(cx, "hosts.new");
    type_into(cx, "hosts.field.name", "db");
    type_into(cx, "hosts.field.address", "10.0.0.6");
    type_into(cx, "hosts.field.user", "postgres");
    click(cx, "hosts.save");

    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(
            view.book().hosts().len(),
            1,
            "a failed write must not add the host to the list"
        );
        assert_eq!(view.book().hosts()[0].name(), "web");
        let message = view
            .hosts_state()
            .message()
            .expect("the failure is explained");
        assert!(message.failed());
        assert!(
            message.text().contains("could not be created")
                || message.text().contains("could not be written"),
            "the reason comes from the layer that failed: {}",
            message.text()
        );
        assert_eq!(
            view.workspace().state(Destination::Hosts),
            SectionState::Populated,
            "the list still shows what the file really holds"
        );
    });
    assert!(cx.debug_bounds("hosts.message").is_some());
}

#[gpui::test]
async fn deleting_a_host_asks_first_and_the_file_follows(cx: &mut TestAppContext) {
    let dir = TempDir::new("delete");
    let store = dir.store();
    seed(
        &store,
        vec![
            stored_host("web", "10.0.0.5", 22, "deploy", None),
            stored_host("db", "10.0.0.6", 22, "postgres", None),
        ],
    );

    let (view, cx) = hosts_view(cx, store);

    click(cx, "hosts.delete.0");
    assert!(
        cx.debug_bounds("hosts.confirm").is_some(),
        "deleting asks first"
    );
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app).book().hosts().len(),
            2,
            "a question deletes nothing"
        );
    });

    click(cx, "hosts.confirm.cancel");
    assert!(
        cx.update(|_window, app| view.read(app).hosts_state().pending_delete().is_none()),
        "cancelling drops the question"
    );
    assert!(
        cx.debug_bounds("hosts.list.2").is_some(),
        "cancelling keeps both hosts listed"
    );

    click(cx, "hosts.delete.0");
    click(cx, "hosts.confirm.delete");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(view.book().hosts().len(), 1);
        assert_eq!(view.workspace().item_count(Destination::Hosts), 1);
        let message = view.hosts_state().message().expect("the result is stated");
        assert!(!message.failed());
    });
    assert!(
        cx.debug_bounds("hosts.list.1").is_some(),
        "one host is left in the list"
    );
    let reloaded = HostStore::at(dir.path()).load().expect("the file loads");
    assert_eq!(reloaded.hosts().len(), 1, "the deletion reached the file");

    // The boundary: the last host can be deleted too, and an empty book is a real
    // result, not a missing capability.
    click(cx, "hosts.delete.0");
    click(cx, "hosts.confirm.delete");
    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(workspace.state(Destination::Hosts), SectionState::Empty);
        assert_eq!(workspace.item_count(Destination::Hosts), 0);
    });
    assert!(
        cx.debug_bounds("hosts.empty").is_some(),
        "an empty book explains itself instead of listing nothing"
    );
    let reloaded = HostStore::at(dir.path())
        .load()
        .expect("an empty book loads");
    assert_eq!(reloaded.hosts().len(), 0);
}

#[gpui::test]
async fn the_host_form_edits_at_the_caret_instead_of_appending_only(cx: &mut TestAppContext) {
    let dir = TempDir::new("caret");
    let (view, cx) = hosts_view(cx, dir.store());

    click(cx, "hosts.new");
    type_into(cx, "hosts.field.name", "web");
    // Home then typing inserts in front of the existing text, and Backspace removes
    // the character before the caret. Appending only would make the field unusable
    // for correcting a saved record.
    click(cx, "hosts.field.name");
    cx.simulate_keystrokes("home");
    cx.simulate_input("my-");
    cx.run_until_parked();
    cx.simulate_keystrokes("end");
    cx.simulate_keystrokes("backspace");
    cx.simulate_input("b");
    cx.run_until_parked();

    cx.update(|_window, app| {
        let draft = view
            .read(app)
            .hosts_state()
            .draft()
            .expect("a form is open")
            .clone();
        assert_eq!(
            draft.text(crate::app::hosts::HostField::Name),
            "my-web",
            "typing at the caret and deleting before it must edit the text in place"
        );
    });
}
// ---------------------------------------------------------------------------
// SSH configuration import (R5)
// ---------------------------------------------------------------------------

/// A configuration with one plain host, one host with an unapplied directive and one
/// host that would need a jump host.
const IMPORT_CONFIG: &str = "\
Host web
    HostName web.example.org
    User deploy
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

Host db
    HostName 10.0.0.6
    User postgres
    ServerAliveInterval 30

Host through-bastion
    HostName internal.example.org
    User root
    ProxyJump bastion.example.org
";

#[gpui::test]
async fn a_proxy_the_preview_could_not_place_is_shown_before_importing(cx: &mut TestAppContext) {
    // The panel must not say the file was read completely while a rule it skipped
    // could still decide where ssh goes.
    let dir = TempDir::new("import-unmatched");
    let config = dir.path().join("config");
    std::fs::write(
        &config,
        "\
Host *.corp
    ProxyJump bastion

Host web
    HostName web.example.org
    User deploy
",
    )
    .expect("the test writes the configuration");
    let (view, cx) = hosts_view(cx, dir.store());
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.open_import_for_test(&config.display().to_string(), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "hosts.import.preview");
    assert!(
        cx.debug_bounds("hosts.import.limits").is_some(),
        "the limits are on screen before anything is applied"
    );
    cx.update(|_window, app| {
        let view = view.read(app);
        let preview = view
            .import_state()
            .preview()
            .expect("the preview is kept while the panel is open");
        assert!(
            preview.is_partial(),
            "a proxy on a pattern no host matches is not a complete read"
        );
        assert_eq!(
            preview.limits().unmatched_proxy().len(),
            1,
            "the skipped block is listed: {:?}",
            preview.limits()
        );
        assert_eq!(preview.limits().unmatched_proxy()[0].keyword(), "*.corp");
        assert_eq!(preview.limits().unmatched_proxy()[0].line(), 1);
        assert_eq!(preview.importable_count(), 1);
        assert!(view.book().hosts().is_empty(), "a preview writes nothing");
    });
}

#[gpui::test]
async fn the_import_panel_opens_on_the_platform_file_and_reads_a_named_one(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("import-open");
    let (view, cx) = hosts_view(cx, dir.store());

    // An empty host list offers the import on its first-run card.
    click(cx, "hosts.import.open");
    assert!(
        cx.debug_bounds("hosts.import.panel").is_some(),
        "the button opens the import panel"
    );
    cx.update(|_window, app| {
        let view = view.read(app);
        let path = view.import_state().path().to_owned();
        assert!(
            path.ends_with("config") || path == shellune_storage::DEFAULT_CONFIG_TEXT,
            "the panel points at the file ssh reads by default: {path}"
        );
        assert!(
            view.import_state().preview().is_none(),
            "nothing was read yet"
        );
    });
}

#[gpui::test]
async fn an_import_preview_shows_its_limits_and_saves_only_the_hosts_it_offered(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("import-apply");
    let config = dir.path().join("config");
    std::fs::write(&config, IMPORT_CONFIG).expect("the test writes the configuration");
    let before = std::fs::read(&config).expect("the configuration is readable");
    let (view, cx) = hosts_view(cx, dir.store());

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.open_import_for_test(&config.display().to_string(), cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("hosts.import.panel").is_some());

    click(cx, "hosts.import.preview");
    assert!(
        cx.debug_bounds("hosts.import.row.0").is_some(),
        "the first host the file describes is listed"
    );
    assert!(
        cx.debug_bounds("hosts.import.row.2").is_some(),
        "the blocked host is listed too, with the reason it is not imported"
    );
    assert!(
        cx.debug_bounds("hosts.import.limits").is_some(),
        "the limits are shown before anything is applied"
    );
    cx.update(|_window, app| {
        let preview = view
            .read(app)
            .import_state()
            .preview()
            .expect("the preview is kept while the panel is open");
        assert_eq!(preview.importable_count(), 2);
        assert_eq!(preview.blocked_count(), 1);
        assert!(preview.is_partial());
        assert!(
            view.read(app).book().hosts().is_empty(),
            "a preview writes nothing"
        );
    });
    assert!(
        !dir.file().exists(),
        "a preview must not create the host file"
    );

    click(cx, "hosts.import.apply");
    cx.update(|_window, app| {
        let view = view.read(app);
        let book = view.book();
        assert_eq!(book.hosts().len(), 2, "the two offered hosts were saved");
        assert!(
            book.hosts()
                .iter()
                .all(|host| host.name() != "through-bastion"),
            "a host behind a jump host is not saved as a direct connection"
        );
        let web = book
            .hosts()
            .iter()
            .find(|host| host.name() == "web")
            .expect("web was imported");
        assert_eq!(web.target(), "deploy@web.example.org:2222");
        assert_eq!(web.identity_file(), Some("~/.ssh/id_ed25519"));
        let message = view.hosts_state().message().expect("the result is stated");
        assert!(
            message.failed(),
            "a blocked host keeps the result from reading as a complete import: {}",
            message.text()
        );
        assert!(message.text().contains('2'), "{}", message.text());
        assert!(
            message.text().contains("through-bastion"),
            "the blocked host stays in the result: {}",
            message.text()
        );
        assert!(
            view.import_state().preview().is_none(),
            "applying closes the preview"
        );
        assert_eq!(
            view.workspace().state(Destination::Hosts),
            SectionState::Populated
        );
        assert_eq!(
            view.workspace().item_count(Destination::Sessions),
            0,
            "importing hosts opens no connection"
        );
    });
    assert!(
        cx.debug_bounds("hosts.row.0").is_some(),
        "the imported hosts are ordinary rows"
    );

    // The SSH client's own file is untouched, and the application record is what the
    // next launch reads.
    let after = std::fs::read(&config).expect("the configuration is still readable");
    assert_eq!(after, before, "the import must not change the source file");
    let reloaded = HostStore::at(dir.path())
        .load()
        .expect("the saved book loads");
    assert_eq!(reloaded.hosts().len(), 2);
}

#[gpui::test]
async fn a_configuration_that_cannot_be_parsed_shows_the_line_and_imports_nothing(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("import-syntax");
    let config = dir.path().join("config");
    std::fs::write(
        &config,
        "Host web\n    HostName web.example.org\n    Port\n",
    )
    .expect("the test writes the configuration");
    let (view, cx) = hosts_view(cx, dir.store());

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.open_import_for_test(&config.display().to_string(), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "hosts.import.preview");

    assert!(
        cx.debug_bounds("hosts.import.error").is_some(),
        "the failure is shown where the preview would be"
    );
    cx.update(|_window, app| {
        let view = view.read(app);
        let error = view
            .import_state()
            .error()
            .expect("the reason the file could not be read is kept");
        assert!(
            error.contains("line 3"),
            "the reason names the line: {error}"
        );
        assert!(
            error.contains("config"),
            "the reason names the file: {error}"
        );
        assert!(view.import_state().preview().is_none());
        assert!(view.book().hosts().is_empty(), "nothing was imported");
    });
    assert!(
        !dir.file().exists(),
        "a refused import must not create the host file"
    );

    // A file that does not exist is the other failure a user can cause.
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.open_import_for_test(&dir.path().join("nope").display().to_string(), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "hosts.import.preview");
    cx.update(|_window, app| {
        let error = view
            .read(app)
            .import_state()
            .error()
            .expect("a missing file is reported");
        assert!(error.contains("nope"), "the reason names the file: {error}");
    });
}

#[gpui::test]
async fn the_grouped_view_gives_every_row_its_own_identity(cx: &mut TestAppContext) {
    let dir = TempDir::new("groups-rows");
    let store = dir.store();
    seed(
        &store,
        vec![
            stored_host("web", "10.0.0.5", 22, "deploy", Some("Prod")),
            stored_host("api", "api.example.org", 2222, "svc", Some("Prod")),
            stored_host("lab", "lab.internal", 22, "root", None),
        ],
    );

    // The grouped view is selected before the first frame, so the flat view never
    // draws a row: what the assertions below see is what this view rendered.
    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_store(cx, workspace, store));
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.select_host_view_for_test(HostView::Groups, cx)
        });
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    assert!(cx.debug_bounds("hosts.group.0").is_some());
    assert!(cx.debug_bounds("hosts.group.1").is_some());
    // The group view lists one row per host, across both groups. Which number each
    // row is drawn under is asserted where it is computed: two rows of one list must
    // never share an element id, and the debug selectors of this GPUI version keep
    // the bounds of earlier frames, so counting here would prove nothing.
    assert!(
        cx.debug_bounds("hosts.list.3").is_some(),
        "both groups together still list all three hosts"
    );
    assert!(cx.debug_bounds("hosts.row.0").is_some());
    assert!(cx.debug_bounds("hosts.row.2").is_some());
}

#[gpui::test]
async fn an_import_whose_write_fails_keeps_the_preview_and_adds_nothing(cx: &mut TestAppContext) {
    let dir = TempDir::new("import-write-fail");
    let config = dir.path().join("config");
    std::fs::write(
        &config,
        "Host web\n    HostName web.example.org\n    User deploy\n",
    )
    .expect("the test writes the configuration");
    let (view, cx) = hosts_view(cx, dir.store());

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.open_import_for_test(&config.display().to_string(), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "hosts.import.preview");
    assert!(cx.debug_bounds("hosts.import.row.0").is_some());

    // The data directory becomes a file, so the next save cannot even create the
    // directory it would write into.
    std::fs::remove_dir_all(dir.path()).expect("the directory is removed");
    std::fs::write(dir.path(), "not a directory").expect("the path becomes a file");

    click(cx, "hosts.import.apply");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(
            view.book().hosts().is_empty(),
            "a failed write must not add the hosts to the list"
        );
        let error = view
            .import_state()
            .error()
            .expect("the reason the write failed is shown");
        assert!(
            error.contains("could not"),
            "the reason comes from the layer that failed: {error}"
        );
        assert!(
            view.import_state().keeps_preview(),
            "the preview survives, so the same hosts can be applied again"
        );
    });
    assert!(cx.debug_bounds("hosts.import.error").is_some());
    assert!(
        cx.debug_bounds("hosts.import.row.0").is_some(),
        "the hosts that could not be written are still listed"
    );
}

#[gpui::test]
async fn a_forward_write_failure_stays_on_the_same_preview_until_the_rules_land(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("import-forward-retry");
    let config = dir.path().join("config");
    std::fs::write(
        &config,
        "\
Host web
    HostName web.example.org
    User deploy
    LocalForward 8080 localhost:80
    RemoteForward 9090 localhost:90
    DynamicForward 1080
",
    )
    .expect("the test writes the configuration");
    let (view, cx) = hosts_view(cx, dir.store());
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.open_import_for_test(&config.display().to_string(), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "hosts.import.preview");

    // The host file can be written. The forwarding file cannot: it is a directory.
    let forward_path = ForwardStore::at(dir.path()).path();
    std::fs::create_dir(&forward_path).expect("the forwarding path blocks the write");

    click(cx, "hosts.import.apply");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(
            view.book().hosts().len(),
            1,
            "the host file was saved before the forwarding write failed"
        );
        assert!(
            view.import_state().is_open() && view.import_state().keeps_preview(),
            "the preview stays so the same rules can be written again"
        );
        let error = view
            .import_state()
            .error()
            .expect("the forwarding failure is shown");
        assert!(
            error.contains("not written"),
            "the message is the forwarding failure, not a completed import: {error}"
        );
        assert!(
            view.hosts_state()
                .message()
                .is_none_or(|message| message.failed()),
            "a partial write is not reported as success"
        );
    });
    assert!(
        ForwardStore::at(dir.path()).load().is_err(),
        "the rules are not in the store while the path cannot be written"
    );

    std::fs::remove_dir(&forward_path).expect("the blocker is removed");
    click(cx, "hosts.import.apply");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(
            view.import_state().preview().is_none(),
            "the preview closes once the rules are stored"
        );
    });
    let rules = ForwardStore::at(dir.path())
        .load()
        .expect("the forwarding file loads after the retry")
        .rules()
        .len();
    assert_eq!(
        rules, 3,
        "the same preview writes the rules the first apply missed"
    );
}

#[gpui::test]
async fn a_finished_import_names_blocked_and_incomplete_hosts(cx: &mut TestAppContext) {
    let dir = TempDir::new("import-withheld");
    let config = dir.path().join("config");
    std::fs::write(
        &config,
        "\
Host web
    HostName web.example.org
    User deploy

Host through-bastion
    HostName internal.example.org
    User root
    ProxyJump bastion.example.org

Host bare
    HostName bare.example.org
",
    )
    .expect("the test writes the configuration");
    let (view, cx) = hosts_view(cx, dir.store());
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.open_import_for_test(&config.display().to_string(), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "hosts.import.preview");
    click(cx, "hosts.import.apply");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(view.book().hosts().len(), 1);
        assert_eq!(view.book().hosts()[0].name(), "web");
        let message = view.hosts_state().message().expect("the result is stated");
        assert!(
            message.failed(),
            "left-out hosts keep this from being a complete success: {}",
            message.text()
        );
        assert!(
            message.text().contains("through-bastion"),
            "the blocked host is named: {}",
            message.text()
        );
        assert!(
            message.text().contains("bare"),
            "the host without a user is named: {}",
            message.text()
        );
        assert!(
            message.text().contains("ProxyJump") && message.text().contains("no User"),
            "each left-out host keeps its reason: {}",
            message.text()
        );
    });
}

// ---------------------------------------------------------------------------
// Remote connections (R4, R6)
// ---------------------------------------------------------------------------

/// Mounts the shell on the hosts destination with its own host store and trust
/// file, so a test never reads or writes the developer's records.
fn hosts_view_with_trust<'a>(
    cx: &'a mut TestAppContext,
    dir: &TempDir,
) -> (Entity<ShelluneView>, &'a mut VisualTestContext) {
    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    (view, cx)
}

/// A presented identity a test can ask about.
fn presented_key(byte: u8) -> shellune_ssh::PresentedKey {
    shellune_ssh::PresentedKey {
        algorithm: "ssh-ed25519".to_owned(),
        fingerprint: shellune_core::sha256_fingerprint(&[byte; 32]).expect("a digest"),
        public_key: "AAAAC3NzaC1lZDI1NTE5AAAAIK6Lk1FqcnKXBppTRsW38vMVUrRnrLv6rQbxs6aDboAs"
            .to_owned(),
    }
}

fn host_prompt(host: &HostConfig, byte: u8) -> shellune_ssh::HostKeyPrompt {
    shellune_ssh::HostKeyPrompt {
        host: host.address().to_owned(),
        port: host.port().get(),
        presented: presented_key(byte),
    }
}

#[gpui::test]
async fn connecting_to_a_saved_host_opens_a_form_with_its_saved_key(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-form");
    let mut web = stored_host("web", "10.0.0.5", 22, "deploy", None);
    web.set_identity_file(Some("/home/deploy/.ssh/id_ed25519"));
    let book = seed(&dir.store(), vec![web]);
    let saved = book.hosts()[0].clone();
    let (view, cx) = hosts_view_with_trust(cx, &dir);

    // Saving a host is not connecting to it: no form is open before the click.
    assert!(cx.debug_bounds("remote.panel").is_none());

    click(cx, "hosts.connect.0");
    assert!(
        cx.debug_bounds("remote.panel").is_some(),
        "the connection form is opened for the host that was chosen"
    );
    assert!(
        cx.debug_bounds("remote.form").is_some(),
        "the method chooser and its fields are on screen"
    );
    cx.update(|_window, app| {
        let state = view
            .read(app)
            .remote_state()
            .expect("a connection form is open");
        assert_eq!(state.method(), MethodChoice::KeyFile);
        assert_eq!(
            state.key_path(),
            saved.identity_file().expect("the saved key path"),
            "the path the host already records is the one the connection offers"
        );
    });
    assert!(
        cx.debug_bounds("remote.key").is_some(),
        "the key path field is shown for the key method"
    );
    assert!(
        cx.debug_bounds("remote.passphrase").is_none(),
        "a plain key does not ask for a passphrase"
    );

    // The other methods are one click away, and each shows the field it needs.
    click(cx, "remote.method.agent");
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app)
                .remote_state()
                .expect("a form is open")
                .method(),
            MethodChoice::Agent,
            "choosing the agent method switches what the form asks for"
        );
    });
    click(cx, "remote.method.password");
    assert!(
        cx.debug_bounds("remote.password").is_some(),
        "the password field belongs to the password method"
    );
    cx.update(|_window, app| {
        assert_eq!(
            view.read(app)
                .remote_state()
                .expect("a form is open")
                .method(),
            MethodChoice::Password
        );
    });
}

#[gpui::test]
async fn the_password_field_is_masked(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-mask");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");

    type_into(cx, "remote.password", "hunter2-correct-horse");
    cx.update(|_window, app| {
        let current = view.read(app);
        let masked = current.remote_field_masked(MethodChoice::Password, app);
        assert!(masked, "a password is typed into a masked field");
        let state = current.remote_state().expect("a form is open");
        assert_eq!(state.password(), "hunter2-correct-horse");
    });
}

#[gpui::test]
async fn an_unknown_identity_is_shown_with_its_algorithm_and_fingerprint(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-trust");
    let book = seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let host = book.hosts()[0].clone();
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.show_trust_prompt(host_prompt(&host, 3), cx);
        });
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("remote.trust").is_some(),
        "an unknown identity is a question, not a connection"
    );
    assert!(
        cx.debug_bounds("remote.trust.fingerprint").is_some(),
        "the fingerprint is on screen where it can be compared"
    );
    cx.update(|_window, app| {
        let state = view.read(app).remote_state().expect("a form is open");
        assert!(
            state.status().is_asking(),
            "the form waits for the decision instead of offering to connect"
        );
        let prompt = state
            .status()
            .prompt()
            .expect("the identity is being asked about");
        assert_eq!(prompt.algorithm(), "ssh-ed25519");
        assert_eq!(prompt.fingerprint(), presented_key(3).fingerprint);
    });
}

#[gpui::test]
async fn the_connection_dialog_names_the_target_and_stage_and_offers_cancel_and_background(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("connect-stage");
    let book = seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let host = book.hosts()[0].clone();
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(Vec::new(), cx);
        });
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("remote.stage").is_some(),
        "the dialog names the stage the attempt is really in"
    );
    assert!(
        cx.debug_bounds("remote.target").is_some(),
        "the dialog names the address it is dialling"
    );
    assert!(
        cx.debug_bounds("remote.waiting").is_none(),
        "connecting is not a wait for the user"
    );
    assert!(
        cx.debug_bounds("remote.cancel").is_some(),
        "an attempt in flight can be ended from the dialog"
    );
    assert!(
        cx.debug_bounds("remote.hide").is_some(),
        "and it can go on in the background"
    );

    // A question from the server is its own stage: the dialog says it is waiting for
    // the user, and still names what it is waiting about and where it is going.
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.show_trust_prompt(host_prompt(&host, 7), cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("remote.waiting").is_some(),
        "the dialog says the connection waits for the user"
    );
    assert!(
        cx.debug_bounds("remote.waiting.detail").is_some(),
        "and says what it is waiting about"
    );
    assert!(
        cx.debug_bounds("remote.target").is_some(),
        "the target is still named while the question is open"
    );

    // Cancelling ends the attempt and takes the form with it: no terminal appears.
    click(cx, "remote.cancel");
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).remote_state().is_none()),
        "the cancelled attempt's form is gone"
    );
    assert!(
        cx.update(|_window, app| view.read(app).open_sessions().is_empty()),
        "nothing was connected, so no session was created"
    );
}

#[gpui::test]
async fn accepting_an_identity_writes_it_and_rejecting_writes_nothing(cx: &mut TestAppContext) {
    let accepted_dir = TempDir::new("remote-accept");
    let book = seed(
        &accepted_dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let host = book.hosts()[0].clone();
    let (view, cx) = hosts_view_with_trust(cx, &accepted_dir);
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.show_trust_prompt(host_prompt(&host, 4), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "remote.trust.accept");

    cx.update(|_window, app| {
        let trust = view.read(app).trust();
        let entry = trust
            .entry("10.0.0.5", 22)
            .expect("the accepted identity is stored");
        assert_eq!(entry.fingerprint(), presented_key(4).fingerprint);
        assert_eq!(entry.algorithm(), "ssh-ed25519");
    });
    let written = TrustStore::at(accepted_dir.path())
        .load()
        .expect("the trust file is readable");
    let comparable = HostKey::new(
        "ssh-ed25519",
        presented_key(4).fingerprint,
        presented_key(4).public_key,
    )
    .expect("a comparable identity");
    assert_eq!(
        written.verdict("10.0.0.5", 22, &comparable),
        shellune_storage::TrustVerdict::Trusted,
        "the decision survives a reload, which is what makes it a decision"
    );

    // The record was written because the attempt that asked the question used the
    // answer, so the answer must have reached it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let answers = cx.update(|_window, app| view.read(app).scripted_answers());
        if !answers.is_empty() {
            assert_eq!(
                answers,
                vec![shellune_ssh::TrustAnswer::Accept],
                "the decision reached the attempt that asked the question"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the acceptance never reached the attempt, so the record would be a claim              nothing used"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // The same question, answered with a refusal, must leave no record.
    let refused_dir = TempDir::new("remote-reject");
    let book = seed(
        &refused_dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let host = book.hosts()[0].clone();
    let (view, cx) = hosts_view_with_trust(cx, &refused_dir);
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.show_trust_prompt(host_prompt(&host, 5), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "remote.trust.reject");

    assert!(
        !refused_dir
            .path()
            .join(shellune_storage::TRUST_FILE_NAME)
            .exists(),
        "a refusal writes no trust record"
    );
    assert!(cx.debug_bounds("remote.failure").is_some());
    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(
            view.pane().is_none(),
            "a refused identity opens no terminal"
        );
        let sessions = view.workspace().sessions();
        let session = sessions.items().last().expect("the attempt was recorded");
        assert!(
            matches!(
                session.state(),
                SessionState::Failed(shellune_core::SessionFailure::HostKeyRejected)
            ),
            "the session says the identity was rejected, got {:?}",
            session.state()
        );
    });
}

#[gpui::test]
async fn a_connection_notice_and_the_first_output_arrive_in_one_batch(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-first-screen");
    let book = seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let host = book.hosts()[0].clone();
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");

    // The shell prints its banner the moment the shell request is accepted, so the
    // connection notice and the first output come out of the channel together.
    let first_screen = "Welcome to the lab\r\n$ ";
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(first_screen.as_bytes().to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    let keep_watching =
        cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    assert!(
        !keep_watching,
        "the attempt is over once the terminal exists"
    );

    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a connection that reached the shell shows a terminal");
    assert!(
        cx.debug_bounds("terminal.viewport").is_some(),
        "the terminal is on screen"
    );
    let screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
    assert!(
        screen.contains("Welcome to the lab"),
        "the output that came with the connection notice is not discarded: {screen:?}"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .remote_state()
            .map(|panel| panel.host().address().to_owned())),
        Some(host.address().to_owned()),
        "the terminal belongs to the host the attempt dialled"
    );
}

#[gpui::test]
async fn an_identity_accepted_after_the_attempt_ended_stores_nothing(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-undelivered");
    let book = seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let host = book.hosts()[0].clone();
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.show_trust_prompt(host_prompt(&host, 9), cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("remote.trust").is_some(),
        "the question is on screen before it is answered"
    );

    // The attempt ends while the question is still up: the answer can no longer reach
    // a connection, so accepting must not leave a record behind.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.end_scripted_attempt(cx));
    });
    click(cx, "remote.trust.accept");
    cx.run_until_parked();

    let stored = TrustStore::at(dir.path())
        .load()
        .expect("the trust file, if it was written at all, is readable");
    assert!(
        stored.entry("10.0.0.5", 22).is_none(),
        "an answer nothing could use must not leave a trust record"
    );
    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(
            view.trust().is_empty(),
            "the identity is not in the trust book either"
        );
        assert!(
            view.pane().is_none(),
            "an attempt that ended opens no terminal"
        );
        assert!(
            view.remote_state()
                .and_then(|state| state.status().failure().cloned())
                .is_some(),
            "the form says the decision could not be delivered"
        );
        let sessions = view.workspace().sessions();
        let session = sessions.items().last().expect("the attempt was recorded");
        assert!(
            matches!(
                session.state(),
                SessionState::Failed(shellune_core::SessionFailure::ConnectionFailed { .. })
            ),
            "an attempt that ended is not a refused identity, got {:?}",
            session.state()
        );
    });
    assert!(cx.debug_bounds("remote.failure").is_some());
}

#[gpui::test]
async fn an_attempt_that_fails_shows_the_reason_and_opens_no_terminal(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-failure");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.fail_remote_for_test(shellune_ssh::ConnectError::AuthRejected, cx);
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("remote.failure").is_some());
    assert!(
        cx.debug_bounds("remote.failure.reason").is_some(),
        "the reason the transport gave is on screen"
    );
    assert!(
        cx.debug_bounds("remote.failure.next").is_some(),
        "the next step is stated, so the failure is actionable"
    );
    assert!(
        cx.debug_bounds("remote.retry").is_some(),
        "a rejected password can be retried with another method"
    );
    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(
            view.pane().is_none(),
            "a failed attempt must not leave a terminal behind"
        );
        let sessions = view.workspace().sessions();
        let session = sessions.items().last().expect("the attempt was recorded");
        assert!(
            matches!(session.state(), SessionState::Failed(_)),
            "the session records the failure, got {:?}",
            session.state()
        );
        assert!(
            !view
                .book()
                .recents()
                .iter()
                .any(|recent| recent.connections() > 0),
            "a failed attempt is not a recent connection"
        );
    });
}

/// A hop rejection names the host and then the credential error.
///
/// That sentence is wider than one line of a 720×480 window at 200% text. It has to
/// wrap inside the dialog, and both it and the next step have to be inside the
/// window: the previous line used to end at "offered" and the rest was past the
/// client edge (R4).
#[gpui::test]
async fn a_rejected_hop_reason_and_its_next_step_fit_the_minimum_window(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-hop-reason");
    seed(
        &dir.store(),
        vec![stored_host("lan109", "192.168.0.109", 22, "root", None)],
    );
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");
    cx.simulate_resize(minimum_window());
    largest_text_size(&view, cx);
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            let host = view.book().hosts()[0].clone();
            let failure = shellune_ssh::ChainFailure::Hop {
                hop: Box::new(shellune_ssh::HopLocation {
                    host_id: host.id(),
                    index: 0,
                    name: host.name().to_owned(),
                    address: host.address().to_owned(),
                    port: host.port().get(),
                    user: host.user().to_owned(),
                }),
                error: Box::new(shellune_ssh::ConnectError::AuthRejected),
            };
            view.fail_chain_for_test(failure, cx);
        });
    });
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    let reason = bounds_of(cx, "remote.failure.reason");
    let next = bounds_of(cx, "remote.failure.next");
    assert_fits_inside(reason, viewport, "remote.failure.reason");
    assert_fits_inside(next, viewport, "remote.failure.next");
    assert!(
        reason.size.height > px(36.0),
        "the hop reason stayed on one line, which is how the window used to cut it: {reason:?}"
    );
    cx.update(|_window, app| {
        let failure = view
            .read(app)
            .remote_state()
            .and_then(|panel| panel.status().failure().cloned())
            .expect("the dialog is showing the hop failure");
        assert!(
            failure.reason().contains("credentials"),
            "the reason is the full rejection: {}",
            failure.reason()
        );
        assert!(
            failure.next_step().contains("authentication method"),
            "the next step is still the credential one: {}",
            failure.next_step()
        );
    });
}

#[gpui::test]
async fn a_changed_identity_is_blocked_without_offering_a_retry(cx: &mut TestAppContext) {
    let dir = TempDir::new("remote-changed");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.fail_remote_for_test(
                shellune_ssh::ConnectError::HostKeyChanged {
                    stored: presented_key(6).fingerprint,
                    presented: presented_key(7),
                },
                cx,
            );
        });
    });
    cx.run_until_parked();

    assert!(cx.debug_bounds("remote.failure").is_some());
    assert!(
        cx.debug_bounds("remote.retry").is_none(),
        "a changed identity is not a button to press again"
    );
    assert!(
        cx.debug_bounds("remote.trust").is_none(),
        "the new identity is never offered for acceptance in the same step"
    );
}

/// Runs the remote poll loop until the panel says the attempt reached a state.
///
/// The poll loop in the application waits on the window's timer, which a test cannot
/// advance, so the same call is driven directly here.
fn pump_remote_until(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    what: &str,
    mut done: impl FnMut(&RemotePanel) -> bool,
) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        cx.update(|_window, app| {
            view.update(app, |view, cx| {
                view.pump_remote_once(cx);
            });
        });
        let reached = cx.update(|_window, app| {
            view.read(app)
                .remote_state()
                .map(&mut done)
                .unwrap_or(false)
        });
        if reached {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the attempt never reached {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[gpui::test]
async fn a_real_ssh_session_opens_a_terminal_that_uses_the_shared_interactions(
    cx: &mut TestAppContext,
) {
    if !shellune_ssh::testserver::Harness::is_available() {
        eprintln!(
            "SKIPPED the SSH session: this host cannot start a disposable OpenSSH server, \
             so no remote terminal is verified here and none is claimed."
        );
        return;
    }
    static SERVER: std::sync::OnceLock<
        Option<std::sync::Mutex<shellune_ssh::testserver::Harness>>,
    > = std::sync::OnceLock::new();
    let server = SERVER.get_or_init(|| {
        shellune_ssh::testserver::Harness::start(
            "app-session",
            shellune_ssh::testserver::Harness::free_port(),
        )
        .ok()
        .map(std::sync::Mutex::new)
    });
    let Some(server) = server.as_ref() else {
        eprintln!("SKIPPED the SSH session: the disposable server could not be started.");
        return;
    };
    let mut server = server.lock().unwrap_or_else(|error| error.into_inner());

    let dir = TempDir::new("remote-session");
    let host = HostConfig::new("lab", "127.0.0.1", server.port(), server.user())
        .expect("a valid test host");
    let host_id = host.id();
    let book = seed(&dir.store(), vec![host]);
    assert_eq!(book.recents().len(), 0);

    let (view, cx) = hosts_view_with_trust(cx, &dir);
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", server.password());
    click(cx, "remote.connect");

    // The first contact asks about the identity, and nothing is stored yet.
    pump_remote_until(cx, &view, "the identity question", |panel| {
        panel.status().is_asking()
    });
    assert!(
        !dir.path().join(shellune_storage::TRUST_FILE_NAME).exists(),
        "an unanswered identity writes nothing"
    );
    click(cx, "remote.trust.accept");

    pump_remote_until(cx, &view, "a live session", |panel| {
        matches!(panel.status(), RemoteStatus::Live)
    });

    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a live connection shows a terminal");
    let (grid, session) = cx.update(|_window, app| {
        let pane = pane.read(app);
        let session = view
            .read(app)
            .workspace()
            .sessions()
            .items()
            .last()
            .map(|session| (session.kind().clone(), session.state().clone()));
        (pane.grid(), session)
    });
    assert!(
        matches!(
            session,
            Some((shellune_core::SessionKind::Remote { host }, SessionState::Connected))
                if host == host_id
        ),
        "the session is the remote one and it is connected, got {session:?}"
    );
    assert!(grid.cols > 0 && grid.rows > 0);
    assert!(
        cx.debug_bounds("terminal.viewport").is_some(),
        "the shared terminal view draws the remote session"
    );

    // Input reaches the remote shell through the same path an input method commits
    // text through, and the answer comes back into the same grid a local shell writes
    // into, in the same UTF-8.
    type_into_pane(cx, &pane, "echo shellune-远程-ok\n");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut screen = String::new();
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| {
                pane.pump_once(cx);
            });
        });
        screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
        if screen.contains("shellune-远程-ok") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        screen.contains("shellune-远程-ok"),
        "the remote shell answered into the grid, got {screen:?}"
    );

    // A window resize has to reach the server's pseudo terminal, not only the grid.
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.pump_once(cx);
        });
    });
    let resized = cx.update(|_window, app| pane.read(app).grid());
    assert_ne!(
        resized, grid,
        "the pane re-measured the grid for the new size"
    );
    type_into_pane(cx, &pane, "stty size\n");
    let expected = format!("{} {}", resized.rows, resized.cols);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut screen = String::new();
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| {
                pane.pump_once(cx);
            });
        });
        screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
        if screen.contains(&expected) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        screen.contains(&expected),
        "the server reports the size the window asked for ({expected:?}), got {screen:?}"
    );

    // The identity is stored and the host is listed as recently connected, which
    // only a connection that really happened can do.
    let written = TrustStore::at(dir.path()).load().expect("the trust file");
    assert_eq!(
        written
            .entry("127.0.0.1", server.port())
            .map(|entry| entry.fingerprint().to_owned()),
        Some(server.host_key_fingerprint()),
        "the accepted identity is the one the server presented"
    );
    cx.update(|_window, app| {
        let view = view.read(app);
        let recent = view.book().recent_of(host_id).expect("a recent record");
        assert_eq!(recent.connections(), 1, "one successful connection");
    });

    // A successful connection now opens its session tab. Close that active tab,
    // rather than clicking the connection panel which is no longer on screen.
    cx.dispatch_action(crate::app::actions::CloseActiveTab);
    cx.run_until_parked();
    cx.update(|_window, app| {
        let view = view.read(app);
        assert!(
            view.pane().is_none(),
            "the terminal is gone with the session"
        );
        assert!(view.remote_state().is_none(), "the panel is closed");
    });
    let state = cx.update(|_window, app| {
        view.read(app)
            .workspace()
            .sessions()
            .items()
            .last()
            .map(|session| session.state().clone())
    });
    assert_eq!(
        state,
        Some(SessionState::Closed),
        "a closed remote session says so"
    );
    server.stop();
}

/// Sends text to a pane through the same entry the input method commits through.
///
/// The pane is mounted inside the shell view here, so a test cannot rely on the
/// window's focus being on the terminal: this calls the commit path directly, which
/// is what an input method does once it hands over the characters.
fn type_into_pane(cx: &mut VisualTestContext, pane: &Entity<TerminalPane>, text: &str) {
    use gpui::EntityInputHandler;

    cx.update(|window, app| {
        pane.update(app, |pane, cx| {
            pane.replace_text_in_range(None, text, window, cx);
        });
    });
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.pump_once(cx);
        });
    });
}

// ---------------------------------------------------------------------------
// Several sessions at once (R7)
// ---------------------------------------------------------------------------

/// Starts the shell the picker points at through the header action, the way a user
/// does, and returns the session it opened.
fn start_local_session(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
) -> Option<SessionId> {
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return None;
    }
    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    cx.update(|_window, app| view.read(app).active_session())
}

/// The state of one session, or None when the shell does not list it.
fn session_state(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
) -> Option<SessionState> {
    cx.update(|_window, app| {
        view.read(app)
            .workspace()
            .session(id)
            .map(|session| session.state().clone())
    })
}

/// Types into one terminal and returns every byte that session sent.
fn sent_after_typing(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
    text: &str,
) -> String {
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the session has a terminal");
    type_into_pane(cx, &pane, text);
    cx.update(|_window, app| String::from_utf8_lossy(pane.read(app).sent()).into_owned())
}

#[gpui::test]
async fn two_local_sessions_open_at_once_and_switch_by_identity(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let Some(first) = start_local_session(cx, &view) else {
        eprintln!("no shell on this machine, skipping");
        return;
    };
    let Some(second) = start_local_session(cx, &view) else {
        return;
    };
    assert_ne!(
        first, second,
        "a second start opens a second session, not the same one"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_sessions()),
        vec![first, second],
        "both sessions stay open, in the order they were started"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(second),
        "the shell just started is the one on screen"
    );

    // Every open session has a tab, and both are really live.
    assert!(cx.debug_bounds("session.tab.0").is_some());
    assert!(cx.debug_bounds("session.tab.1").is_some());
    assert_eq!(
        session_state(cx, &view, first),
        Some(SessionState::Connected)
    );
    assert_eq!(
        session_state(cx, &view, second),
        Some(SessionState::Connected),
        "opening a second terminal must not end the first one"
    );
    assert!(
        cx.debug_bounds("session.context").is_some(),
        "the selected session is described on screen"
    );

    // Input goes to the selected terminal and to no other.
    let second_sent = sent_after_typing(cx, &view, second, "second-only");
    assert!(second_sent.contains("second-only"));
    let first_pane = cx
        .update(|_window, app| view.read(app).pane_of(first).cloned())
        .expect("the first terminal");
    assert!(
        cx.update(|_window, app| first_pane.read(app).sent().is_empty()),
        "typing into one session reached another one"
    );

    // Switching by tab moves the selection and the keyboard with it.
    click(cx, "session.tab.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(first)
    );
    let first_sent = sent_after_typing(cx, &view, first, "first-only");
    assert!(first_sent.contains("first-only"));
    assert!(
        !first_sent.contains("second-only"),
        "the terminals keep their own input: {first_sent:?}"
    );
}

#[gpui::test]
async fn closing_one_session_leaves_the_others_running(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let Some(first) = start_local_session(cx, &view) else {
        return;
    };
    let Some(second) = start_local_session(cx, &view) else {
        return;
    };

    click(cx, "session.tab.close.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_sessions()),
        vec![second],
        "only the closed session's terminal goes away"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(second),
        "the remaining session takes over"
    );
    assert_eq!(
        session_state(cx, &view, first),
        Some(SessionState::Closed),
        "a closed session is recorded as closed"
    );
    assert_eq!(
        session_state(cx, &view, second),
        Some(SessionState::Connected),
        "and the other session keeps running"
    );

    // The surviving terminal still takes input.
    let sent = sent_after_typing(cx, &view, second, "still-here");
    assert!(sent.contains("still-here"), "{sent:?}");
}

#[gpui::test]
async fn a_session_that_ended_reconnects_without_replaying_anything(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let Some(id) = start_local_session(cx, &view) else {
        return;
    };
    let old_pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the terminal of the started shell");
    let old_tag = cx.update(|_window, app| old_pane.read(app).tag());

    // Leave the shell the way a user does, so the session really ends.
    cx.simulate_input("exit");
    cx.simulate_keystrokes("enter");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        cx.update(|_window, app| {
            old_pane.update(app, |pane, cx| {
                pane.pump_once(cx);
            });
        });
        let ended =
            cx.update(|_window, app| !matches!(old_pane.read(app).state(), PaneState::Live));
        if ended {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the shell never exited"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    cx.run_until_parked();
    assert!(
        matches!(
            session_state(cx, &view, id),
            Some(SessionState::Disconnected(_))
        ),
        "an exited shell is listed as ended, not as running"
    );

    // The manual reconnect starts another process for the same session.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.reconnect_session(id, cx));
    });
    cx.run_until_parked();

    let new_pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the reconnected session has a terminal");
    assert_ne!(
        new_pane.entity_id(),
        old_pane.entity_id(),
        "a reconnect is a new terminal, not the old one restarted"
    );
    assert_eq!(
        session_state(cx, &view, id),
        Some(SessionState::Connected),
        "the new process is running"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .session(id)
            .map(|session| session.generation())),
        Some(1),
        "the session counts its processes so late events can be told apart"
    );
    assert!(
        cx.update(|_window, app| new_pane.read(app).sent().is_empty()),
        "nothing typed into the old shell is sent to the new one"
    );

    // An event that belongs to the old process is dropped instead of being drawn into
    // the terminal that replaced it.
    let accepted = cx.update(|_window, app| {
        new_pane.update(app, |pane, cx| {
            pane.adopt_events(
                old_tag,
                vec![SourceEvent::Data(b"stale-old-command\r\n".to_vec())],
                cx,
            )
        })
    });
    assert!(
        !accepted,
        "an event of the previous process must be refused"
    );
    let screen = cx.update(|_window, app| new_pane.read(app).visible_text().join("\n"));
    assert!(
        !screen.contains("stale-old-command"),
        "the old process output reached the new terminal: {screen:?}"
    );
}

#[gpui::test]
async fn a_pending_connection_keeps_navigation_and_the_other_session_usable(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("multi-pending");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let workspace = Workspace::new();
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let Some(local) = start_local_session(cx, &view) else {
        return;
    };

    // Start a connection that stays pending: the form opens, the attempt reports
    // nothing yet.
    click(cx, "nav.hosts");
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.script_remote_attempt(Vec::new(), cx));
    });
    cx.run_until_parked();
    let pending = cx
        .update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .find(|session| session.kind().is_remote())
                .map(|session| session.state().clone())
        })
        .expect("the attempt is listed as a session");
    assert!(
        pending.is_pending(),
        "an attempt that has not answered is still connecting, got {pending:?}"
    );

    // The pending session has a tab of its own, so switching is possible before it is
    // live.
    assert!(
        cx.debug_bounds("session.tab.1").is_some(),
        "the connecting session is listed beside the local one"
    );

    // Navigation keeps working while the attempt waits.
    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(
        destination_of(&view, cx),
        Destination::LocalShells,
        "a pending connection does not pin the navigation"
    );
    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);

    // The live session still answers, and the attempt is still being watched.
    let sent = sent_after_typing(cx, &view, local, "while-connecting");
    assert!(sent.contains("while-connecting"), "{sent:?}");
    assert_eq!(
        session_state(cx, &view, local),
        Some(SessionState::Connected)
    );
    let watching = cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    assert!(watching, "the attempt is still being polled");
}

#[gpui::test]
async fn a_dropped_remote_session_changes_only_its_own_state(cx: &mut TestAppContext) {
    let dir = TempDir::new("multi-drop");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let workspace = Workspace::new();
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let Some(local) = start_local_session(cx, &view) else {
        return;
    };
    click(cx, "nav.hosts");
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"remote ready\r\n$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.pump_remote_once(cx);
        });
    });
    cx.run_until_parked();

    let remote = cx
        .update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .find(|session| {
                    session.kind().is_remote() && matches!(session.state(), SessionState::Connected)
                })
                .map(|session| session.id())
        })
        .expect("the remote session is live");
    let remote_pane = cx
        .update(|_window, app| view.read(app).pane_of(remote).cloned())
        .expect("the remote terminal exists");
    let tag = cx.update(|_window, app| remote_pane.read(app).tag());

    // An event addressed to another session is refused by this terminal.
    let foreign = EventTag::new(local, 0);
    let accepted = cx.update(|_window, app| {
        remote_pane.update(app, |pane, cx| {
            pane.adopt_events(foreign, vec![SourceEvent::Data(b"leaked\r\n".to_vec())], cx)
        })
    });
    assert!(!accepted, "an event of another session must be refused");
    let screen = cx.update(|_window, app| remote_pane.read(app).visible_text().join("\n"));
    assert!(
        !screen.contains("leaked"),
        "output of another session was drawn: {screen:?}"
    );

    // The connection dropping changes the state of that session only.
    let accepted = cx.update(|_window, app| {
        remote_pane.update(app, |pane, cx| {
            pane.adopt_events(
                tag,
                vec![SourceEvent::Lost("the channel was closed".to_owned())],
                cx,
            )
        })
    });
    assert!(accepted, "an event of this session is applied");
    cx.run_until_parked();

    assert!(
        matches!(
            session_state(cx, &view, remote),
            Some(SessionState::Disconnected(_))
        ),
        "the dropped session says it was disconnected, got {:?}",
        session_state(cx, &view, remote)
    );
    assert_eq!(
        session_state(cx, &view, local),
        Some(SessionState::Connected),
        "the local session did not notice the remote one dropping"
    );
    let sent = sent_after_typing(cx, &view, local, "still-responsive");
    assert!(
        sent.contains("still-responsive"),
        "the other session kept answering: {sent:?}"
    );
}

#[gpui::test]
async fn closing_every_tab_leaves_no_session_behind(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let Some(first) = start_local_session(cx, &view) else {
        return;
    };
    let Some(second) = start_local_session(cx, &view) else {
        return;
    };

    // Closing through the tabs is the user's path, and it is what has to leave the
    // screen tidy: no strip, no tab, and both sessions recorded as closed.
    click(cx, "session.tab.close.1");
    click(cx, "session.tab.close.0");
    assert!(
        cx.update(|_window, app| view.read(app).open_sessions())
            .is_empty(),
        "no terminal outlives its tab"
    );
    for id in [first, second] {
        assert_eq!(session_state(cx, &view, id), Some(SessionState::Closed));
    }
    assert!(
        cx.update(|_window, app| view.read(app).open_tabs())
            .is_empty(),
        "the tab strip goes away with the last session"
    );
}

#[gpui::test]
async fn two_remote_hosts_stay_open_side_by_side(cx: &mut TestAppContext) {
    let dir = TempDir::new("multi-hosts");
    seed(
        &dir.store(),
        vec![
            stored_host("alpha", "10.0.0.5", 22, "deploy", None),
            stored_host("beta", "10.0.0.6", 22, "deploy", None),
        ],
    );
    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    // Two hosts, two connections, each with the banner of its own server. The form is
    // opened for each of them in turn, which is what the connect button does; the
    // button itself is covered by the single-host tests.
    let hosts = cx.update(|_window, app| view.read(app).workspace().hosts().items().to_vec());
    assert_eq!(hosts.len(), 2);
    for host in &hosts {
        let banner = format!("{} ready", host.name());
        cx.update(|_window, app| {
            view.update(app, |view, cx| view.open_remote(host, cx));
        });
        assert_eq!(
            cx.update(|_window, app| view
                .read(app)
                .remote_state()
                .map(|panel| panel.host().name().to_owned())),
            Some(host.name().to_owned()),
            "the form is for the host it was opened on"
        );
        cx.update(|_window, app| {
            view.update(app, |view, cx| {
                view.script_remote_attempt(
                    vec![
                        shellune_ssh::RemoteEvent::Connected,
                        shellune_ssh::RemoteEvent::Data(format!("{banner}\r\n$ ").into_bytes()),
                    ],
                    cx,
                );
            });
        });
        cx.run_until_parked();
        cx.update(|_window, app| {
            view.update(app, |view, cx| {
                view.pump_remote_once(cx);
            });
        });
        cx.run_until_parked();
    }

    let remotes: Vec<(SessionId, String)> = cx.update(|_window, app| {
        view.read(app)
            .workspace()
            .sessions()
            .items()
            .iter()
            .filter(|session| session.kind().is_remote())
            .map(|session| (session.id(), session.title().to_owned()))
            .collect()
    });
    assert_eq!(remotes.len(), 2, "both connections stay open: {remotes:?}");
    let titles: Vec<&str> = remotes.iter().map(|(_, title)| title.as_str()).collect();
    assert!(
        titles.contains(&"alpha") && titles.contains(&"beta"),
        "each tab names the host it talks to: {titles:?}"
    );
    assert!(
        cx.debug_bounds("session.tab.1").is_some(),
        "two remote sessions are two tabs"
    );

    // The two terminals are separate: each shows its own server's banner and none of
    // the other's.
    for (index, (id, title)) in remotes.iter().enumerate() {
        let pane = cx
            .update(|_window, app| view.read(app).pane_of(*id).cloned())
            .expect("each remote session has its own terminal");
        let screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
        let own = format!("{title} ready");
        let other = remotes
            .iter()
            .map(|(_, known)| format!("{known} ready"))
            .find(|banner| *banner != own)
            .expect("the other server's banner");
        assert!(
            screen.contains(&own),
            "session {index} lost the screen of its own server: {screen:?}"
        );
        assert!(
            !screen.contains(&other),
            "session {index} shows the screen of the other server: {screen:?}"
        );
    }

    // Switching between them moves the terminal on screen, not the connections.
    let (first, _) = remotes[0].clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.activate(first, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(first)
    );
    assert_eq!(
        destination_of(&view, cx),
        Destination::LocalShells,
        "both remote sessions stay in the workspace"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_sessions())
            .len(),
        2,
        "switching does not close either session"
    );
}

#[gpui::test]
async fn a_remote_session_reconnects_through_the_form_without_replay(cx: &mut TestAppContext) {
    let dir = TempDir::new("multi-remote-reconnect");
    seed(
        &dir.store(),
        vec![stored_host("alpha", "10.0.0.5", 22, "deploy", None)],
    );
    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"first process\r\n$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.pump_remote_once(cx);
        });
    });
    cx.run_until_parked();

    let id = cx
        .update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .find(|session| session.kind().is_remote())
                .map(|session| session.id())
        })
        .expect("the remote session exists");
    let old_pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the remote terminal exists");
    let tag = cx.update(|_window, app| old_pane.read(app).tag());

    // The server hangs up.
    cx.update(|_window, app| {
        old_pane.update(app, |pane, cx| {
            pane.adopt_events(
                tag,
                vec![SourceEvent::Lost("the channel was closed".to_owned())],
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(matches!(
        session_state(cx, &view, id),
        Some(SessionState::Disconnected(_))
    ));

    // Reconnecting a remote session needs credentials again, so the form comes back
    // with everything that is not a secret; the session is not claimed as connecting
    // before the form has started an attempt.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.reconnect_session(id, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).remote_state().is_some()),
        "the connection form is open again"
    );
    assert!(matches!(
        session_state(cx, &view, id),
        Some(SessionState::Disconnected(_))
    ));
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        vec![id],
        "the tab keeps its place instead of becoming a second session"
    );

    // The new attempt reuses the record, so there is still one tab for this host.
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"second process\r\n$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.pump_remote_once(cx);
        });
    });
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        vec![id],
        "the reconnect did not open a second tab"
    );
    let new_pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the reconnected session has a terminal");
    assert_ne!(new_pane.entity_id(), old_pane.entity_id());
    assert_eq!(session_state(cx, &view, id), Some(SessionState::Connected));
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .session(id)
            .map(|session| session.generation())),
        Some(1)
    );
    assert!(
        cx.update(|_window, app| new_pane.read(app).sent().is_empty()),
        "nothing typed into the lost connection is replayed"
    );
    let screen = cx.update(|_window, app| new_pane.read(app).visible_text().join("\n"));
    assert!(screen.contains("second process"), "{screen:?}");
    assert!(
        !screen.contains("first process"),
        "the new terminal starts empty instead of replaying the old screen: {screen:?}"
    );
}

#[gpui::test]
async fn leaving_the_application_closes_every_session(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let Some(first) = start_local_session(cx, &view) else {
        return;
    };
    let Some(second) = start_local_session(cx, &view) else {
        return;
    };

    // This is the cleanup the window runs when it closes.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.close_all_sessions(cx));
    });
    cx.run_until_parked();

    assert!(
        cx.update(|_window, app| view.read(app).open_sessions())
            .is_empty(),
        "no terminal outlives the window"
    );
    for id in [first, second] {
        assert_eq!(
            session_state(cx, &view, id),
            Some(SessionState::Closed),
            "every session is closed when the application leaves"
        );
    }
}
#[gpui::test]
async fn a_reconnect_that_can_never_work_offers_close_instead(cx: &mut TestAppContext) {
    // The reachable path: a remote session that is disconnected, and whose saved host is
    // deleted before the user presses reconnect. Nothing about that can be fixed by
    // another attempt, so the session must stop advertising reconnect and name the step
    // that works - closing it (R12).
    let dir = TempDir::new("multi-reconnect-refused");
    seed(
        &dir.store(),
        vec![stored_host("alpha", "10.0.0.5", 22, "deploy", None)],
    );
    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"live\r\n$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.pump_remote_once(cx);
        });
    });
    cx.run_until_parked();

    let id = cx
        .update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .find(|session| session.kind().is_remote())
                .map(|session| session.id())
        })
        .expect("the remote session exists");
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the remote terminal exists");
    let tag = cx.update(|_window, app| pane.read(app).tag());
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.adopt_events(
                tag,
                vec![SourceEvent::Lost("the channel was closed".to_owned())],
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert!(matches!(
        session_state(cx, &view, id),
        Some(SessionState::Disconnected(_))
    ));

    // The user deletes the host the session was using.
    click(cx, "nav.hosts");
    click(cx, "hosts.delete.0");
    click(cx, "hosts.confirm.delete");
    assert_eq!(
        cx.update(|_window, app| view.read(app).book().hosts().len()),
        0,
        "the host is gone"
    );
    show_remote_terminal(cx, &view, id);

    // Pressing reconnect now refuses, and the refusal is recorded as a reason no attempt
    // can resolve.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.reconnect_session(id, cx));
    });
    cx.run_until_parked();
    let state = session_state(cx, &view, id);
    assert!(
        matches!(
            state,
            Some(SessionState::Disconnected(
                shellune_core::SessionFailure::ReconnectUnavailable { .. }
            ))
        ),
        "a refused reconnect keeps its reason, got {state:?}"
    );

    // What the user sees: the reason, and close as the step - not a reconnect button that
    // would only refuse again.
    assert!(
        cx.debug_bounds("terminal.failure").is_some(),
        "the reason the session cannot come back is stated"
    );
    assert!(
        cx.debug_bounds("session.close").is_some(),
        "closing the finished session is offered"
    );
    // The button's absence is checked through the rule that draws it: the test window
    // never forgets a selector once a frame drew it (GPUI clears every per-frame buffer
    // but the debug bounds), and this button was drawn before the host was deleted.
    assert!(
        !cx.update(|_window, app| view.read(app).offers_reconnect(id)),
        "a session that can never be reconnected must not offer the button"
    );

    // The step that was offered is the step that works.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.close_session(id, cx));
    });
    cx.run_until_parked();
    assert_eq!(session_state(cx, &view, id), Some(SessionState::Closed));
}

// ---------------------------------------------------------------------------
// The file view of a remote session (R9, R12)
// ---------------------------------------------------------------------------

/// The answers a file view receives in a test, and the requests it made.
#[derive(Clone, Default)]
struct FileAnswers(std::sync::Arc<std::sync::Mutex<FileScript>>);

#[derive(Default)]
struct FileScript {
    sent: Vec<shellune_core::BrowseRequest>,
    answers: Vec<shellune_core::BrowseResponse>,
    accepting: bool,
}

impl FileAnswers {
    /// A channel that accepts requests.
    fn accepting() -> Self {
        let answers = Self::default();
        answers.0.lock().expect("a free lock").accepting = true;
        answers
    }

    /// A channel whose connection is already gone.
    fn closed() -> Self {
        Self::default()
    }

    /// Answers one request with a real-looking directory.
    fn reply(&self, request: u64, path: &str, entries: &[(&str, shellune_core::FileKind)]) {
        let entries = entries
            .iter()
            .filter_map(|(name, kind)| shellune_core::DirEntry::new(*name, *kind, None))
            .collect();
        self.0
            .lock()
            .expect("a free lock")
            .answers
            .push(shellune_core::BrowseResponse::Listed {
                request,
                path: path.to_owned(),
                entries,
            });
    }

    /// Answers one request with the report of a change.
    fn changed(&self, request: u64, outcome: shellune_core::MutationOutcome) {
        self.0
            .lock()
            .expect("a free lock")
            .answers
            .push(shellune_core::BrowseResponse::Changed { request, outcome });
    }

    /// Answers one request with a failure.
    fn fail(&self, request: u64, failure: shellune_core::BrowseFailure) {
        self.0
            .lock()
            .expect("a free lock")
            .answers
            .push(shellune_core::BrowseResponse::Failed { request, failure });
    }

    /// Everything the view asked for so far.
    fn requests(&self) -> Vec<shellune_core::BrowseRequest> {
        self.0.lock().expect("a free lock").sent.clone()
    }
}

impl crate::app::files::BrowseChannel for FileAnswers {
    fn send(&mut self, request: shellune_core::BrowseRequest) -> bool {
        let mut script = self.0.lock().expect("a free lock");
        script.sent.push(request);
        script.accepting
    }

    fn drain(&mut self) -> Vec<shellune_core::BrowseResponse> {
        std::mem::take(&mut self.0.lock().expect("a free lock").answers)
    }
}

/// Mounts the shell on a store holding one host, as the connection tests do.
fn remote_view(cx: &mut TestAppContext) -> (Entity<ShelluneView>, &mut VisualTestContext) {
    let dir = TempDir::new("files-view");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let workspace = Workspace::new();
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    (view, cx)
}

/// Connects the seeded host through a scripted attempt and returns its session.
fn live_remote(cx: &mut VisualTestContext, view: &Entity<ShelluneView>) -> SessionId {
    click(cx, "nav.hosts");
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"remote ready\r\n$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.pump_remote_once(cx);
        });
    });
    cx.run_until_parked();
    let id = cx
        .update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .find(|session| {
                    session.kind().is_remote() && matches!(session.state(), SessionState::Connected)
                })
                .map(|session| session.id())
        })
        .expect("the remote session is live");
    show_remote_terminal(cx, view, id);
    id
}

/// Shows a remote session's terminal the way a user does: by clicking its tab.
///
/// Coming from the hosts page lands on the workspace. The sessions page reached from
/// the sidebar lists the sessions instead.
fn show_remote_terminal(cx: &mut VisualTestContext, view: &Entity<ShelluneView>, id: SessionId) {
    let index = cx
        .update(|_window, app| view.read(app).open_tabs())
        .iter()
        .position(|tab| *tab == id)
        .expect("the session has a tab");
    // `click` takes a selector that lives for the whole test run.
    let selector: &'static str = Box::leak(format!("session.tab.{index}").into_boxed_str());
    click(cx, selector);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(id),
        "the tab shows that session"
    );
}

/// Connects the seeded host again and returns the session that is new.
///
/// [live_remote] answers with the first connected session, which is the right answer
/// only while there is one; a test that needs a second one has to say which it already
/// has.
fn live_remote_second(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    known: SessionId,
) -> SessionId {
    click(cx, "nav.hosts");
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"second ready\r\n$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.pump_remote_once(cx);
        });
    });
    cx.run_until_parked();
    let id = cx
        .update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .find(|session| {
                    session.id() != known
                        && session.kind().is_remote()
                        && matches!(session.state(), SessionState::Connected)
                })
                .map(|session| session.id())
        })
        .expect("the second remote session is live");
    show_remote_terminal(cx, view, id);
    id
}

/// Applies what the file views reported, the way the poll loop does.
fn pump_files(cx: &mut VisualTestContext, view: &Entity<ShelluneView>) {
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.pump_files_for_test(cx);
        });
    });
    cx.run_until_parked();
}

#[gpui::test]
async fn the_file_view_names_its_host_and_shows_the_directory_the_server_listed(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);

    let answers = FileAnswers::accepting();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(
                id,
                "web",
                "deploy@10.0.0.5:22",
                Box::new(answers.clone()),
                cx,
            );
        });
    });
    cx.run_until_parked();

    // Nothing is browsed before the user asks for it.
    assert!(
        cx.debug_bounds("files.panel").is_none(),
        "a session that was not opened for files shows no file view"
    );
    assert!(
        cx.debug_bounds("session.files").is_some(),
        "a connection that can browse offers the file view"
    );

    click(cx, "session.files");
    assert!(
        cx.debug_bounds("files.panel").is_some(),
        "the file view is drawn beside the terminal"
    );
    assert!(
        cx.debug_bounds("files.loading").is_some(),
        "nothing is claimed before the server answers"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .files_browser(id)
            .and_then(|browser| { browser.path().map(str::to_owned) })),
        None,
        "no path is invented while the answer is missing"
    );
    let sent = answers.requests();
    assert_eq!(sent.len(), 1, "one request was made: {sent:?}");
    assert_eq!(
        sent[0],
        shellune_core::BrowseRequest::Home { request: 1 },
        "the first request asks the server for the login directory"
    );

    answers.reply(
        1,
        "/home/deploy",
        &[
            ("docs", shellune_core::FileKind::Directory),
            ("文档 与 空格", shellune_core::FileKind::Directory),
            ("notes.txt", shellune_core::FileKind::File),
        ],
    );
    pump_files(cx, &view);

    assert!(
        cx.debug_bounds("files.host").is_some(),
        "the host is always named beside the path"
    );
    assert!(cx.debug_bounds("files.path").is_some());
    assert!(
        cx.debug_bounds("files.entry.0").is_some() && cx.debug_bounds("files.entry.2").is_some(),
        "every entry the server reported is listed"
    );
    assert!(
        cx.debug_bounds("files.parent").is_some(),
        "a directory that has a parent offers the way up"
    );
    // The file view shares the panel with the terminal without pushing it out of the
    // window, which is what keeps the terminal usable while a directory is open.
    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_fits_inside(bounds_of(cx, "files.panel"), viewport, "file view");
    assert_fits_inside(
        bounds_of(cx, "terminal.viewport"),
        viewport,
        "terminal grid",
    );
    let state = cx
        .update(|_window, app| {
            view.read(app)
                .files_browser(id)
                .and_then(|browser| browser.state().listing().cloned())
        })
        .expect("a real listing is shown");
    assert_eq!(state.path(), "/home/deploy");
    let names: Vec<&str> = state.entries().iter().map(|entry| entry.name()).collect();
    assert_eq!(
        names,
        vec!["docs", "文档 与 空格", "notes.txt"],
        "directories first, and a Chinese name with spaces is kept as it is"
    );

    // Opening a directory asks the server for exactly that path.
    click(cx, "files.entry.0");
    let sent = answers.requests();
    assert_eq!(
        sent.last(),
        Some(&shellune_core::BrowseRequest::List {
            request: 2,
            path: "/home/deploy/docs".to_owned(),
        }),
        "the entry is opened as the path it is in: {sent:?}"
    );
    answers.reply(
        2,
        "/home/deploy/docs",
        &[("deep.txt", shellune_core::FileKind::File)],
    );
    pump_files(cx, &view);
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .files_browser(id)
            .and_then(|browser| browser.path().map(str::to_owned))),
        Some("/home/deploy/docs".to_owned())
    );

    // The parent comes back through the same connection.
    click(cx, "files.up");
    let sent = answers.requests();
    assert_eq!(
        sent.last(),
        Some(&shellune_core::BrowseRequest::List {
            request: 3,
            path: "/home/deploy".to_owned(),
        }),
        "going up asks for the parent of the path that is really open"
    );
    answers.reply(
        3,
        "/home/deploy",
        &[("docs", shellune_core::FileKind::Directory)],
    );
    pump_files(cx, &view);
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .files_browser(id)
            .and_then(|browser| browser.path().map(str::to_owned))),
        Some("/home/deploy".to_owned())
    );

    // A refused directory is stated, and the terminal is still the live session it
    // was: an SFTP failure is not a disconnected shell.
    click(cx, "files.refresh");
    answers.fail(
        4,
        shellune_core::BrowseFailure::PermissionDenied {
            path: "/home/deploy".to_owned(),
        },
    );
    pump_files(cx, &view);
    assert!(
        cx.debug_bounds("files.failure").is_some(),
        "the failure is drawn where the listing was"
    );
    assert!(
        cx.debug_bounds("files.failure.reason").is_some(),
        "the reason the server gave is part of the failure"
    );
    assert!(
        cx.update(|_window, app| view
            .read(app)
            .files_browser(id)
            .is_some_and(|browser| browser.state().listing().is_none())),
        "a failed directory is not kept as a listing"
    );
    assert_eq!(
        session_state(cx, &view, id),
        Some(SessionState::Connected),
        "a directory that cannot be read never marks the terminal as ended"
    );
    assert!(
        cx.debug_bounds("terminal.failure").is_none(),
        "the terminal shows no failure notice of its own"
    );
}

#[gpui::test]
async fn a_connection_that_cannot_browse_says_so_without_ending_the_session(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(
                id,
                "web",
                "deploy@10.0.0.5:22",
                Box::new(FileAnswers::closed()),
                cx,
            );
        });
    });
    cx.run_until_parked();
    click(cx, "session.files");

    // The request never left the process, so the view says so at once instead of
    // waiting for an answer that cannot come.
    assert!(
        cx.debug_bounds("files.failure").is_some(),
        "a connection that cannot take the request is reported"
    );
    assert_eq!(
        session_state(cx, &view, id),
        Some(SessionState::Connected),
        "the terminal is untouched by a file view that has no connection"
    );

    // Closing the view leaves the terminal and the session exactly as they were.
    click(cx, "files.close");
    assert!(
        !cx.update(|_window, app| view.read(app).files_visible(id)),
        "closing the file view leaves the session without one"
    );
    assert_eq!(session_state(cx, &view, id), Some(SessionState::Connected));

    // Reopening starts from nowhere: the previous attempt left no path behind.
    click(cx, "session.files");
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .files_browser(id)
            .and_then(|browser| browser.path().map(str::to_owned))),
        None,
        "a reopened view asks for the login directory again"
    );
}
// ---------------------------------------------------------------------------
// Changing the remote directory (R11)
// ---------------------------------------------------------------------------

/// Opens the file view of a live session on a directory with two entries.
///
/// The listing comes from a real answer, because a change is only ever asked for
/// inside a directory the server really listed.
fn open_files(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
) -> FileAnswers {
    let answers = FileAnswers::accepting();
    let channel = answers.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(id, "web", "deploy@10.0.0.5:22", Box::new(channel), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "session.files");
    answers.reply(
        1,
        "/home/deploy",
        &[
            ("docs", shellune_core::FileKind::Directory),
            ("notes.txt", shellune_core::FileKind::File),
        ],
    );
    pump_files(cx, view);
    answers
}

/// Opens both sources on scripted listings, so the local column is not this computer's home.
fn open_both_sources(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
    local_path: &str,
    local_entries: &[(&str, shellune_core::FileKind)],
) -> (FileAnswers, FileAnswers) {
    let remote = FileAnswers::accepting();
    let local = FileAnswers::accepting();
    let remote_channel = remote.clone();
    let local_channel = local.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(
                id,
                "web",
                "deploy@10.0.0.5:22",
                Box::new(remote_channel),
                cx,
            );
            view.attach_local_files_for_test(id, Box::new(local_channel), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "session.files");
    remote.reply(
        1,
        "/home/deploy",
        &[
            ("docs", shellune_core::FileKind::Directory),
            ("notes.txt", shellune_core::FileKind::File),
        ],
    );
    local.reply(1, local_path, local_entries);
    pump_files(cx, view);
    (remote, local)
}

/// The names the view shows, in the order it lists them.
fn listed_names(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
) -> Vec<String> {
    cx.update(|_window, app| {
        view.read(app)
            .files_browser(id)
            .and_then(|browser| browser.state().listing().cloned())
            .map(|listing| {
                listing
                    .entries()
                    .iter()
                    .map(|entry| entry.name().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    })
}

#[gpui::test]
async fn a_new_folder_is_asked_for_and_the_result_is_the_servers_next_listing(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let answers = open_files(cx, &view, id);
    let before = answers.requests().len();

    click(cx, "files.new");
    assert!(
        cx.debug_bounds("files.question").is_some(),
        "a new folder is asked for before anything is sent"
    );
    assert_eq!(
        answers.requests().len(),
        before,
        "opening the question sends no request"
    );
    type_into(cx, "files.question.input", "文档 新");
    assert_eq!(
        answers.requests().len(),
        before,
        "typing a name sends no request either"
    );

    click(cx, "files.question.confirm");
    let sent = answers.requests();
    assert_eq!(
        sent.last(),
        Some(&shellune_core::BrowseRequest::CreateDirectory {
            request: 2,
            directory: "/home/deploy".to_owned(),
            name: "文档 新".to_owned(),
        }),
        "the name is sent exactly as typed, inside the directory on screen: {sent:?}"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).files_question_open()),
        "the answered question is gone"
    );
    assert!(
        cx.update(|_window, app| view
            .read(app)
            .files_browser(id)
            .is_some_and(shellune_core::FileBrowser::is_changing)),
        "the panel knows the change is with the server"
    );
    assert_eq!(
        listed_names(cx, &view, id),
        vec!["docs".to_owned(), "notes.txt".to_owned()],
        "the list is still the server's previous answer, not a guess"
    );

    // The server applied it. What the view shows next is the server's own listing.
    answers.changed(
        2,
        shellune_core::MutationOutcome::new(
            shellune_core::MutationAction::CreateDirectory {
                path: "/home/deploy/文档 新".to_owned(),
            },
            vec![shellune_core::MutationStep::done(
                "/home/deploy/文档 新",
                Some(shellune_core::FileKind::Directory),
            )],
        ),
    );
    pump_files(cx, &view);
    let sent = answers.requests();
    assert_eq!(
        sent.last(),
        Some(&shellune_core::BrowseRequest::List {
            request: 3,
            path: "/home/deploy".to_owned(),
        }),
        "the directory is read again instead of being assumed: {sent:?}"
    );
    answers.reply(
        3,
        "/home/deploy",
        &[
            ("docs", shellune_core::FileKind::Directory),
            ("文档 新", shellune_core::FileKind::Directory),
            ("notes.txt", shellune_core::FileKind::File),
        ],
    );
    pump_files(cx, &view);
    assert_eq!(
        listed_names(cx, &view, id),
        vec![
            "docs".to_owned(),
            "文档 新".to_owned(),
            "notes.txt".to_owned()
        ],
        "the list after the change is the one the server reported"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).toast_titles()),
        vec![
            crate::app::copy::Locale::English
                .strings()
                .files_change_done
                .to_owned()
        ],
        "what the server did is announced in the corner, not left in the list"
    );
    assert!(
        !cx.update(|_window, app| view
            .read(app)
            .files_browser(id)
            .is_some_and(shellune_core::FileBrowser::is_changing)),
        "the change is no longer in flight"
    );
}

#[gpui::test]
async fn an_invalid_name_or_a_collision_never_reaches_the_server(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let answers = open_files(cx, &view, id);
    let before = answers.requests().len();

    click(cx, "files.new");
    type_into(cx, "files.question.input", "a/b");
    click(cx, "files.question.confirm");
    assert_eq!(
        answers.requests().len(),
        before,
        "a name with a separator would be another path, so nothing is sent"
    );
    assert!(
        cx.debug_bounds("files.question.problem").is_some(),
        "the reason is shown beside the name"
    );
    assert!(
        cx.debug_bounds("files.question").is_some(),
        "the question stays open so the name can be corrected"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_text().map(str::to_owned)),
        Some("a/b".to_owned()),
        "the name the user typed stays in the field beside the reason"
    );

    // The same for a name that is already in the listing.
    clear_field(cx, "files.question.input", "a/b".chars().count());
    type_into(cx, "files.question.input", "docs");
    click(cx, "files.question.confirm");
    assert_eq!(
        answers.requests().len(),
        before,
        "a name that is taken is a conflict, not a request"
    );
    assert!(cx.debug_bounds("files.question.problem").is_some());
}

#[gpui::test]
async fn the_delete_question_names_the_target_and_a_partial_failure_is_reported(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let answers = open_files(cx, &view, id);
    let before = answers.requests().len();

    // The first row is the directory, so the question is about a recursive delete.
    // An entry with its own row and its own two actions fits in the panel: a question
    // about a target the user cannot see is not a confirmation of anything.
    let panel = bounds_of(cx, "files.panel");
    let row = bounds_of(cx, "files.row.0");
    assert!(
        row.size.width <= panel.size.width,
        "the row of an entry stays inside the file view: {row:?} vs {panel:?}"
    );
    assert!(
        bounds_of(cx, "files.delete.0").size.width <= panel.size.width,
        "the delete button of a row is inside the file view"
    );

    click(cx, "files.delete.0");
    assert!(
        cx.debug_bounds("files.question").is_some(),
        "deleting asks first"
    );
    assert!(
        cx.debug_bounds("files.question.target").is_some(),
        "the full path is part of the question"
    );
    assert!(
        cx.debug_bounds("files.question.kind").is_some(),
        "what is being deleted is part of the question"
    );
    assert!(
        cx.debug_bounds("files.question.note").is_some(),
        "a directory takes its contents with it, and the question says so"
    );

    // Cancelling sends nothing at all.
    click(cx, "files.question.cancel");
    assert!(
        !cx.update(|_window, app| view.read(app).files_question_open()),
        "the question is closed"
    );
    assert_eq!(
        answers.requests().len(),
        before,
        "cancelling a delete sends no change"
    );
    assert_eq!(
        listed_names(cx, &view, id),
        vec!["docs".to_owned(), "notes.txt".to_owned()],
        "the list did not change either"
    );

    click(cx, "files.delete.0");
    click(cx, "files.question.confirm");
    let sent = answers.requests();
    assert_eq!(
        sent.last(),
        Some(&shellune_core::BrowseRequest::Delete {
            request: 2,
            path: "/home/deploy/docs".to_owned(),
            kind: shellune_core::FileKind::Directory,
        }),
        "the confirmed delete names the path and the kind: {sent:?}"
    );

    // The server removed one item and refused another, which is reported as such.
    answers.changed(
        2,
        shellune_core::MutationOutcome::new(
            shellune_core::MutationAction::Delete {
                path: "/home/deploy/docs".to_owned(),
                kind: shellune_core::FileKind::Directory,
            },
            vec![
                shellune_core::MutationStep::done(
                    "/home/deploy/docs/readme.txt",
                    Some(shellune_core::FileKind::File),
                ),
                shellune_core::MutationStep::failed(
                    "/home/deploy/docs/locked.txt",
                    Some(shellune_core::FileKind::File),
                    shellune_core::BrowseFailure::PermissionDenied {
                        path: "/home/deploy/docs/locked.txt".to_owned(),
                    },
                ),
                shellune_core::MutationStep::failed(
                    "/home/deploy/docs",
                    Some(shellune_core::FileKind::Directory),
                    shellune_core::BrowseFailure::Refused {
                        reason: "an item inside this directory was not deleted".to_owned(),
                    },
                ),
            ],
        ),
    );
    pump_files(cx, &view);
    assert!(
        cx.debug_bounds("files.report").is_some(),
        "the result is reported"
    );
    assert!(
        cx.debug_bounds("files.report.step.0").is_some()
            && cx.debug_bounds("files.report.step.1").is_some()
            && cx.debug_bounds("files.report.step.2").is_some(),
        "every item the server reached is listed, not just the first failure"
    );

    // The list that follows is the server's, not the report's.
    answers.reply(
        3,
        "/home/deploy",
        &[
            ("docs", shellune_core::FileKind::Directory),
            ("notes.txt", shellune_core::FileKind::File),
        ],
    );
    pump_files(cx, &view);
    assert_eq!(
        listed_names(cx, &view, id),
        vec!["docs".to_owned(), "notes.txt".to_owned()],
        "the partially deleted directory is still there, because the server still has it"
    );
}

#[gpui::test]
async fn a_rename_sends_both_names_and_the_list_follows_the_server(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let answers = open_files(cx, &view, id);

    click(cx, "files.rename.1");
    assert!(
        cx.debug_bounds("files.question.target").is_some(),
        "an entry is named by its full path before it is renamed"
    );
    clear_field(cx, "files.question.input", "notes.txt".chars().count());
    type_into(cx, "files.question.input", "说明.txt");
    click(cx, "files.question.confirm");
    assert_eq!(
        answers.requests().last(),
        Some(&shellune_core::BrowseRequest::Rename {
            request: 2,
            from: "/home/deploy/notes.txt".to_owned(),
            to: "/home/deploy/说明.txt".to_owned(),
        }),
        "both paths are named in the same directory"
    );

    answers.changed(
        2,
        shellune_core::MutationOutcome::new(
            shellune_core::MutationAction::Rename {
                from: "/home/deploy/notes.txt".to_owned(),
                to: "/home/deploy/说明.txt".to_owned(),
            },
            vec![shellune_core::MutationStep::done(
                "/home/deploy/说明.txt",
                Some(shellune_core::FileKind::File),
            )],
        ),
    );
    pump_files(cx, &view);
    answers.reply(
        3,
        "/home/deploy",
        &[
            ("docs", shellune_core::FileKind::Directory),
            ("说明.txt", shellune_core::FileKind::File),
        ],
    );
    pump_files(cx, &view);
    assert_eq!(
        listed_names(cx, &view, id),
        vec!["docs".to_owned(), "说明.txt".to_owned()],
        "the list after the rename is the server's own answer"
    );
    assert!(
        cx.update(|_window, app| view.read(app).toast_titles())
            .iter()
            .any(|title| title
                == crate::app::copy::Locale::English
                    .strings()
                    .files_change_done),
        "the change the server applied is announced"
    );
}

#[gpui::test]
async fn a_change_of_another_request_never_reaches_this_directory(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let answers = open_files(cx, &view, id);

    // An answer with a number this view never asked for is dropped, like any other
    // late answer: the directory on screen keeps the listing that really belongs to it.
    answers.changed(
        99,
        shellune_core::MutationOutcome::new(
            shellune_core::MutationAction::Delete {
                path: "/home/other/secret".to_owned(),
                kind: shellune_core::FileKind::File,
            },
            vec![shellune_core::MutationStep::done(
                "/home/other/secret",
                Some(shellune_core::FileKind::File),
            )],
        ),
    );
    pump_files(cx, &view);
    assert!(
        cx.debug_bounds("files.report").is_none(),
        "a change report of another request is not shown here"
    );
    assert_eq!(
        listed_names(cx, &view, id),
        vec!["docs".to_owned(), "notes.txt".to_owned()],
        "the listing of this host is untouched"
    );
}

#[gpui::test]
async fn a_session_that_ends_takes_its_file_view_and_the_polling_with_it(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let _answers = open_files(cx, &view, id);
    assert!(
        cx.debug_bounds("files.panel").is_some(),
        "the file view is open"
    );

    // The connection ends while the file view is open.
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the terminal exists");
    let tag = cx.update(|_window, app| pane.read(app).tag());
    let accepted = cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.adopt_events(
                tag,
                vec![SourceEvent::Lost("the channel was closed".to_owned())],
                cx,
            )
        })
    });
    assert!(accepted, "the event belongs to this session");
    cx.run_until_parked();

    assert!(
        !cx.update(|_window, app| view.read(app).files_visible(id)),
        "a listing nothing backs is not kept after the connection ends"
    );
    let keeps_polling =
        cx.update(|_window, app| view.update(app, |view, cx| view.pump_files_for_test(cx)));
    assert!(
        !keeps_polling,
        "polling stops once no browsable session is left"
    );
    // The panel is drawn only while a view is open, so the model above is what decides
    // whether it is there at all; the tab stops offering it for the same reason.
    assert!(
        !cx.update(|_window, app| view.read(app).files_can_browse(id)),
        "a connection that ended offers no file view any more"
    );
}

#[gpui::test]
async fn a_deep_path_is_readable_instead_of_being_cut_off(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let answers = open_files(cx, &view, id);
    let short = bounds_of(cx, "files.path");

    let deep = format!("/home/deploy/{}/inner", "a-long-directory-name".repeat(5));
    click(cx, "files.refresh");
    answers.reply(2, &deep, &[("inner", shellune_core::FileKind::Directory)]);
    pump_files(cx, &view);
    let long = bounds_of(cx, "files.path");
    assert!(
        long.size.width <= bounds_of(cx, "files.panel").size.width,
        "the path stays inside the panel"
    );
    assert!(
        long.size.height > short.size.height,
        "a deep path takes more lines instead of losing its end: {short:?} vs {long:?}"
    );
}

#[gpui::test]
async fn both_columns_name_their_own_source_and_a_narrow_window_switches(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_remote, _local) = open_both_sources(
        cx,
        &view,
        id,
        "/C:/work",
        &[
            ("空目录", shellune_core::FileKind::Directory),
            ("报告.txt", shellune_core::FileKind::File),
        ],
    );

    assert!(
        cx.debug_bounds("files.panel").is_some(),
        "the remote column is drawn"
    );
    assert!(
        cx.debug_bounds("files.local.panel").is_some(),
        "the local column is drawn beside it"
    );
    assert!(cx.debug_bounds("files.source").is_some());
    assert!(cx.debug_bounds("files.local.source").is_some());
    assert!(cx.debug_bounds("files.host").is_some());
    assert!(cx.debug_bounds("files.local.host").is_some());
    assert!(cx.debug_bounds("files.path").is_some());
    assert!(cx.debug_bounds("files.local.path").is_some());
    assert!(
        cx.debug_bounds("files.show.local").is_none(),
        "a wide window shows both sources instead of a switch"
    );
    let (remote_host, local_host, remote_path, local_path) = cx.update(|_window, app| {
        let view = view.read(app);
        let remote = view.files_browser(id).expect("remote");
        let local = view.files_local_browser(id).expect("local");
        (
            remote.host().to_owned(),
            local.host().to_owned(),
            remote.path().unwrap().to_owned(),
            local.path().unwrap().to_owned(),
        )
    });
    assert_eq!(remote_host, "web");
    assert_ne!(
        local_host, remote_host,
        "the local machine is not the remote host"
    );
    assert_eq!(remote_path, "/home/deploy");
    assert_eq!(local_path, "/C:/work");
    assert_ne!(remote_path, local_path);
}

#[gpui::test]
async fn a_narrow_window_switches_source_without_merging_them(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    cx.simulate_resize(size(px(760.0), px(720.0)));
    cx.run_until_parked();
    let (remote, local) = open_both_sources(
        cx,
        &view,
        id,
        "/C:/work",
        &[("报告.txt", shellune_core::FileKind::File)],
    );
    assert!(
        cx.debug_bounds("files.panel").is_some(),
        "the narrow window starts on the remote source"
    );
    assert!(
        cx.debug_bounds("files.local.panel").is_none(),
        "the local source is not merged into the remote column"
    );
    assert!(cx.debug_bounds("files.show.local").is_some());
    assert!(cx.debug_bounds("files.show.remote").is_some());
    assert_eq!(remote.requests().len(), 1);
    assert_eq!(
        local.requests().len(),
        1,
        "the hidden source was still opened"
    );
    click(cx, "files.show.local");
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_shown(id)),
        Some(crate::app::files::FileSource::Local)
    );
    assert!(cx.debug_bounds("files.local.panel").is_some());
    assert!(cx.debug_bounds("files.local.host").is_some());
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .files_browser(id)
                .and_then(|browser| browser.path().map(str::to_owned))
        }),
        Some("/home/deploy".to_owned()),
        "switching source does not retarget the hidden one"
    );
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .files_local_browser(id)
                .and_then(|browser| browser.path().map(str::to_owned))
        }),
        Some("/C:/work".to_owned())
    );
    click(cx, "files.show.remote");
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_shown(id)),
        Some(crate::app::files::FileSource::Remote)
    );
    assert_eq!(remote.requests().len(), 1);
    assert_eq!(local.requests().len(), 1);
}

#[gpui::test]
async fn two_hundred_percent_file_columns_stay_inside_the_window(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_remote, _local) = open_both_sources(
        cx,
        &view,
        id,
        "/C:/work",
        &[("报告.txt", shellune_core::FileKind::File)],
    );
    focus_root(&view, cx);
    cx.dispatch_action(CycleTextSize);
    cx.dispatch_action(CycleTextSize);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest
    );

    // 720 and 1120 cannot hold two scaled columns plus a terminal. 2400 can:
    // expanded rail 416 + two columns 1360 + terminal reserve 384 = 2160.
    for (window, dual) in [
        (minimum_window(), false),
        (design_window(), false),
        (size(px(2400.0), px(900.0)), true),
    ] {
        cx.simulate_resize(window);
        cx.run_until_parked();
        let viewport = cx.update(|window, _app| window.viewport_size());
        assert_file_column_inside(cx, "files.panel", viewport);
        assert_file_column_inside(cx, "files.path", viewport);
        assert_file_column_inside(cx, "files.refresh", viewport);
        assert_file_column_inside(cx, "files.close", viewport);
        let mode = if dual {
            "files.layout.dual"
        } else {
            "files.layout.single"
        };
        assert!(
            cx.debug_bounds(mode).is_some(),
            "file columns use {mode} in {viewport:?}"
        );
        if dual {
            assert_file_column_inside(cx, "files.local.panel", viewport);
            assert_file_column_inside(cx, "files.local.path", viewport);
            assert_file_column_inside(cx, "files.local.refresh", viewport);
        } else {
            assert_file_column_inside(cx, "files.show.local", viewport);
            assert_file_column_inside(cx, "files.show.remote", viewport);
        }
    }

    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    click(cx, "files.show.local");
    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_shown(id)),
        Some(crate::app::files::FileSource::Local)
    );
    assert!(
        cx.debug_bounds("files.layout.single").is_some(),
        "the minimum window stays on one source after the switch"
    );
    assert_file_column_inside(cx, "files.local.panel", viewport);
    assert_file_column_inside(cx, "files.local.path", viewport);
    assert_file_column_inside(cx, "files.local.refresh", viewport);
    assert_file_column_inside(cx, "files.close", viewport);
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .files_browser(id)
                .and_then(|browser| browser.path().map(str::to_owned))
        }),
        Some("/home/deploy".to_owned()),
        "the hidden remote path stays the remote directory"
    );
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .files_local_browser(id)
                .and_then(|browser| browser.path().map(str::to_owned))
        }),
        Some("/C:/work".to_owned()),
        "showing the local column does not retarget it"
    );
}

fn assert_file_column_inside(
    cx: &mut VisualTestContext,
    selector: &'static str,
    viewport: Size<Pixels>,
) {
    assert_within_width(bounds_of(cx, selector), viewport, selector);
}

#[gpui::test]
async fn file_actions_have_a_button_path_and_a_drop_uses_that_same_request(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (remote, local) = open_both_sources(
        cx,
        &view,
        id,
        "/C:/work",
        &[
            ("空目录", shellune_core::FileKind::Directory),
            ("报告.txt", shellune_core::FileKind::File),
        ],
    );
    let transfers = TransferAnswers::accepting();
    let channel = transfers.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_transfers_for_test(id, Box::new(channel), cx);
        });
    });
    cx.run_until_parked();

    // Keyboard rename needs a selected row. Refresh is separate, and a modified key is not a file action.
    let before = local.requests().len();
    let handled = cx.update(|window, app| {
        view.update(app, |view, cx| {
            let ignored = view.files_key_for_test(
                id,
                crate::app::files::FileSource::Local,
                "c",
                true,
                window,
                cx,
            );
            view.files_key_for_test(
                id,
                crate::app::files::FileSource::Local,
                "down",
                false,
                window,
                cx,
            );
            let rename = view.files_key_for_test(
                id,
                crate::app::files::FileSource::Local,
                "f2",
                false,
                window,
                cx,
            );
            (!ignored, rename)
        })
    });
    assert_eq!(handled, (true, true));
    assert!(
        cx.update(|_window, app| view.read(app).files_question_open()),
        "f2 asks to rename the selected entry"
    );
    assert_eq!(
        local.requests().len(),
        before,
        "the rename question sends nothing"
    );
    click(cx, "files.question.cancel");
    assert!(!cx.update(|_window, app| view.read(app).files_question_open()));
    let refreshed = cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.files_key_for_test(
                id,
                crate::app::files::FileSource::Local,
                "f5",
                false,
                window,
                cx,
            )
        })
    });
    assert!(refreshed, "f5 refreshes the focused source");
    assert_eq!(local.requests().len(), before + 1, "f5 is the only request");
    assert!(matches!(
        local.requests().last(),
        Some(shellune_core::BrowseRequest::List { .. })
    ));
    assert_eq!(
        remote.requests().len(),
        1,
        "the remote source was not refreshed"
    );

    local.reply(
        2,
        "/C:/work",
        &[
            ("空目录", shellune_core::FileKind::Directory),
            ("报告.txt", shellune_core::FileKind::File),
        ],
    );
    pump_files(cx, &view);

    // A new folder is not drawn until this computer lists the directory again.
    let before = local.requests().len();
    click(cx, "files.local.new");
    type_into(cx, "files.question.input", "日志");
    click(cx, "files.question.confirm");
    assert!(matches!(
        local.requests().last(),
        Some(shellune_core::BrowseRequest::CreateDirectory { name, .. }) if name == "日志"
    ));
    assert_eq!(remote.requests().len(), 1);
    assert!(
        !cx.update(|_window, app| {
            view.read(app)
                .files_local_browser(id)
                .and_then(|browser| browser.state().listing())
                .is_some_and(|listing| listing.entries().iter().any(|entry| entry.name() == "日志"))
        }),
        "the new folder is not shown before the directory is read again"
    );
    local.changed(
        before as u64 + 1,
        shellune_core::MutationOutcome::new(
            shellune_core::MutationAction::CreateDirectory {
                path: "/C:/work/日志".to_owned(),
            },
            vec![shellune_core::MutationStep::done(
                "/C:/work/日志",
                Some(shellune_core::FileKind::Directory),
            )],
        ),
    );
    pump_files(cx, &view);
    local.reply(
        before as u64 + 2,
        "/C:/work",
        &[
            ("日志", shellune_core::FileKind::Directory),
            ("空目录", shellune_core::FileKind::Directory),
            ("报告.txt", shellune_core::FileKind::File),
        ],
    );
    pump_files(cx, &view);
    assert!(cx.update(|_window, app| {
        view.read(app)
            .files_local_browser(id)
            .and_then(|browser| browser.state().listing())
            .is_some_and(|listing| listing.entries().iter().any(|entry| entry.name() == "日志"))
    }));

    // Cancelling a delete sends nothing. Confirming does, and the list waits for the refresh.
    let before = local.requests().len();
    click(cx, "files.local.delete.2");
    assert!(cx.debug_bounds("files.question.target").is_some());
    assert!(cx.debug_bounds("files.question.kind").is_some());
    assert!(cx.debug_bounds("files.question.note").is_some());
    click(cx, "files.question.cancel");
    assert_eq!(
        local.requests().len(),
        before,
        "cancelling a delete sends nothing"
    );
    click(cx, "files.local.rename.2");
    clear_field(cx, "files.question.input", "报告.txt".chars().count());
    type_into(cx, "files.question.input", "说明.txt");
    click(cx, "files.question.confirm");
    assert!(matches!(
        local.requests().last(),
        Some(shellune_core::BrowseRequest::Rename { to, .. }) if to.ends_with("说明.txt")
    ));

    // Upload and download buttons, and a drop, start the same kind of transfer.
    click(cx, "files.local.to_remote.2");
    click(cx, "files.to_local.1");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_drop_for_test(
                id,
                crate::app::files::FileSource::Remote,
                crate::app::files::FileSource::Local,
                "/C:/work",
                "报告.txt",
                cx,
            );
            view.files_drop_for_test(
                id,
                crate::app::files::FileSource::Local,
                crate::app::files::FileSource::Remote,
                "/home/deploy",
                "notes.txt",
                cx,
            );
        });
    });
    cx.run_until_parked();
    let starts = transfers.starts();
    assert_eq!(starts.len(), 4, "two buttons and two drops: {starts:?}");
    let folder = crate::app::files::local_protocol_to_os("/C:/work").expect("a local folder");
    let uploaded = folder.join("报告.txt").to_string_lossy().into_owned();
    let downloaded = crate::app::transfers::local_path(&folder, "notes.txt")
        .to_string_lossy()
        .into_owned();
    assert_eq!(starts[0].direction, TransferDirection::Upload);
    assert_eq!(starts[0].source, uploaded);
    assert_eq!(starts[0].destination, "/home/deploy/报告.txt");
    assert_eq!(starts[2].direction, TransferDirection::Upload);
    assert_eq!(starts[2].source, starts[0].source);
    assert_eq!(starts[2].destination, starts[0].destination);
    assert_eq!(starts[1].direction, TransferDirection::Download);
    assert_eq!(starts[1].source, "/home/deploy/notes.txt");
    assert_eq!(starts[1].destination, downloaded);
    assert_eq!(starts[3].source, starts[1].source);
    assert_eq!(starts[3].destination, starts[1].destination);
}

#[gpui::test]
async fn an_empty_or_failed_source_leaves_the_other_source_and_the_terminal(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (remote, local) = open_both_sources(cx, &view, id, "/C:/work/empty", &[]);

    assert!(
        cx.debug_bounds("files.local.empty").is_some(),
        "an empty directory says it is empty"
    );
    assert!(
        cx.debug_bounds("files.entry.0").is_some(),
        "the remote listing stays"
    );
    assert!(cx.update(|_window, app| {
        view.read(app)
            .files_local_browser(id)
            .and_then(|browser| browser.state().listing())
            .is_some_and(|listing| listing.is_empty())
    }));
    assert_eq!(session_state(cx, &view, id), Some(SessionState::Connected));

    click(cx, "files.local.refresh");
    local.fail(
        2,
        shellune_core::BrowseFailure::PermissionDenied {
            path: "/C:/work/empty".to_owned(),
        },
    );
    pump_files(cx, &view);
    assert!(cx.debug_bounds("files.local.failure").is_some());
    assert!(cx.debug_bounds("files.local.failure.reason").is_some());
    assert!(cx.debug_bounds("files.local.failure.refresh").is_some());
    assert!(
        cx.debug_bounds("files.failure.reconnect").is_none(),
        "a local permission failure is not a lost SSH connection"
    );
    assert!(
        cx.update(|_window, app| {
            view.read(app)
                .files_browser(id)
                .and_then(|browser| browser.state().listing())
                .is_some()
        }),
        "the remote listing is untouched"
    );
    assert_eq!(session_state(cx, &view, id), Some(SessionState::Connected));
    assert!(cx.debug_bounds("terminal.failure").is_none());

    click(cx, "files.local.failure.refresh");
    local.fail(
        3,
        shellune_core::BrowseFailure::NotFound {
            path: "/C:/work/missing".to_owned(),
        },
    );
    pump_files(cx, &view);
    assert!(cx.update(|_window, app| {
        view.read(app)
            .files_local_browser(id)
            .and_then(|browser| browser.state().failure())
            .is_some_and(|failure| matches!(failure, shellune_core::BrowseFailure::NotFound { .. }))
    }));

    click(cx, "files.refresh");
    remote.fail(
        2,
        shellune_core::BrowseFailure::ConnectionLost {
            reason: "the SSH connection is no longer open".to_owned(),
        },
    );
    pump_files(cx, &view);
    assert!(cx.debug_bounds("files.failure").is_some());
    assert!(
        cx.debug_bounds("files.failure.reconnect").is_none(),
        "a connected session does not offer a reconnect that cannot start"
    );
    assert!(
        cx.debug_bounds("files.failure.refresh").is_some(),
        "refresh stays available for this source"
    );
    assert!(cx.debug_bounds("files.failure.up").is_some());
    assert!(
        cx.debug_bounds("files.failure.refresh.hint").is_some(),
        "the card says to refresh this source instead of reconnecting the session"
    );
    assert!(
        cx.update(|_window, app| {
            view.read(app)
                .files_local_browser(id)
                .and_then(|browser| browser.state().failure())
                .is_some_and(|failure| {
                    matches!(failure, shellune_core::BrowseFailure::NotFound { .. })
                })
        }),
        "the remote disconnect does not replace the local failure"
    );
    let before = remote.requests().len();
    click(cx, "files.failure.refresh");
    assert!(
        matches!(
            remote.requests().last(),
            Some(shellune_core::BrowseRequest::List { .. })
        ),
        "refresh asks this source to list again: {:?}",
        remote.requests()
    );
    assert_eq!(remote.requests().len(), before + 1);
    assert_eq!(session_state(cx, &view, id), Some(SessionState::Connected));
    assert!(cx.debug_bounds("terminal.failure").is_none());
    assert!(
        cx.update(|_window, app| {
            view.read(app)
                .files_local_browser(id)
                .and_then(|browser| browser.state().failure())
                .is_some_and(|failure| {
                    matches!(failure, shellune_core::BrowseFailure::NotFound { .. })
                })
        }),
        "refreshing the remote source leaves the local failure as it was"
    );
}

// ---------------------------------------------------------------------------
// Transfers (R10, R17)
// ---------------------------------------------------------------------------

/// The commands a transfer view sent in a test, and the events it receives.
#[derive(Clone, Default)]
struct TransferAnswers(std::sync::Arc<std::sync::Mutex<TransferScript>>);

#[derive(Default)]
struct TransferScript {
    sent: Vec<shellune_ssh::TransferCommand>,
    events: Vec<shellune_core::TransferEvent>,
    accepting: bool,
}

impl TransferAnswers {
    fn accepting() -> Self {
        let answers = Self::default();
        answers.0.lock().expect("a free lock").accepting = true;
        answers
    }

    fn push(&self, event: shellune_core::TransferEvent) {
        self.0.lock().expect("a free lock").events.push(event);
    }

    fn commands(&self) -> Vec<shellune_ssh::TransferCommand> {
        self.0.lock().expect("a free lock").sent.clone()
    }

    /// The tasks the window asked the connection to start.
    fn starts(&self) -> Vec<shellune_ssh::TransferRequest> {
        self.commands()
            .into_iter()
            .filter_map(|command| match command {
                shellune_ssh::TransferCommand::Start(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    fn decisions(&self) -> Vec<(shellune_core::TransferId, shellune_core::OverwriteChoice)> {
        self.commands()
            .into_iter()
            .filter_map(|command| match command {
                shellune_ssh::TransferCommand::Decide { transfer, choice } => {
                    Some((transfer, choice))
                }
                _ => None,
            })
            .collect()
    }

    fn cancels(&self) -> Vec<shellune_core::TransferId> {
        self.commands()
            .into_iter()
            .filter_map(|command| match command {
                shellune_ssh::TransferCommand::Cancel { transfer } => Some(transfer),
                _ => None,
            })
            .collect()
    }
}

impl crate::app::transfers::TransferChannel for TransferAnswers {
    fn send(&mut self, command: shellune_ssh::TransferCommand) -> bool {
        let mut script = self.0.lock().expect("a free lock");
        script.sent.push(command);
        script.accepting
    }

    fn drain(&mut self) -> Vec<shellune_core::TransferEvent> {
        std::mem::take(&mut self.0.lock().expect("a free lock").events)
    }
}

/// Opens the file view of a live remote session and attaches a transfer channel.
fn open_transfers(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
) -> (FileAnswers, TransferAnswers) {
    let files = open_files(cx, view, id);
    let transfers = TransferAnswers::accepting();
    let channel = transfers.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_transfers_for_test(id, Box::new(channel), cx);
        });
    });
    cx.run_until_parked();
    (files, transfers)
}

#[gpui::test]
async fn an_upload_asks_for_the_file_and_then_reports_the_task_it_started(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    assert!(
        !cx.update(|_window, app| view.read(app).transfers_strip_shown(id)),
        "with nothing transferred the strip takes no room"
    );

    // The question is asked before anything is sent.
    click(cx, "files.upload");
    assert!(
        cx.debug_bounds("files.question").is_some(),
        "the upload question names its target"
    );
    assert!(
        transfers.starts().is_empty(),
        "answering the question is what starts a task, not opening it"
    );

    // Typing the local path and confirming creates a real task on the connection.
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/报告 终稿.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();

    let starts = transfers.starts();
    assert_eq!(starts.len(), 1, "one task was started: {starts:?}");
    assert_eq!(starts[0].source, "C:/work/报告 终稿.txt");
    assert_eq!(
        starts[0].destination, "/home/deploy/报告 终稿.txt",
        "the file keeps its own name in the directory on screen"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        1
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_tasks().len()),
        1,
        "the list now holds the task instead of the empty note"
    );
    assert!(
        cx.debug_bounds("transfers.row.0").is_some(),
        "the task is drawn with its source and target"
    );
}

#[gpui::test]
async fn a_transfer_question_copies_over_the_chosen_protocol_and_only_a_chosen_directory(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    // The remote side is a field of its own and both protocols are offered.
    assert!(
        cx.debug_bounds("files.question.remote").is_some(),
        "the remote directory can be typed, which is what a server without SFTP needs"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_protocol()),
        Some(shellune_core::TransferProtocol::Sftp),
        "a host without a choice copies over SFTP"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("/home/deploy".to_owned()),
        "the question starts at the directory on screen"
    );
    assert!(
        cx.debug_bounds("files.question.recursive").is_none(),
        "SFTP copies a file, so it has no directory to offer"
    );

    // Switching to SCP is a decision about this task, and it sends nothing.
    click(cx, "files.question.protocol.scp");
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_protocol()),
        Some(shellune_core::TransferProtocol::Scp)
    );
    assert!(
        cx.debug_bounds("files.question.recursive").is_some(),
        "SCP can copy a directory, so it offers the choice"
    );
    assert!(
        cx.debug_bounds("files.question.protocol.note").is_some(),
        "the question says what choosing SCP changes"
    );
    assert!(
        transfers.starts().is_empty(),
        "switching protocols starts nothing"
    );

    // A directory named without the recursive choice is refused before a task exists.
    let directory = std::env::temp_dir().join(format!("shellune-question-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    let directory = directory.to_string_lossy().into_owned();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test(&directory);
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        transfers.starts().is_empty(),
        "a directory is not copied as a whole unless that was chosen"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_text().map(str::to_owned)),
        Some(directory.clone()),
        "the path that was refused stays in the field"
    );

    // Choosing recursion copies the tree, over the protocol the question carries.
    click(cx, "files.question.recursive");
    assert!(cx.update(|_window, app| view.read(app).files_question_recursive()));
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test(&directory);
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let starts = transfers.starts();
    assert_eq!(starts.len(), 1, "the chosen copy started: {starts:?}");
    assert_eq!(starts[0].protocol, shellune_core::TransferProtocol::Scp);
    assert!(starts[0].recursive, "the tree was the choice");
    assert!(
        starts[0].destination.starts_with("/home/deploy/"),
        "the file lands in the remote directory the question named: {}",
        starts[0].destination
    );

    // The remote directory is the one that was typed, not the one on screen.
    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_remote_for_test("/srv/uploads");
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let starts = transfers.starts();
    assert_eq!(
        starts.last().map(|start| start.destination.as_str()),
        Some("/srv/uploads/report.txt"),
        "the typed directory is where the file goes"
    );
}

#[gpui::test]
async fn a_host_saved_with_scp_starts_its_transfers_with_scp(cx: &mut TestAppContext) {
    let dir = TempDir::new("scp-default");
    let mut seeded = stored_host("legacy", "10.0.0.9", 22, "deploy", None);
    seeded.set_transfer_protocol(shellune_core::TransferProtocol::Scp);
    seed(&dir.store(), vec![seeded]);
    let workspace = Workspace::new();
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_protocol()),
        Some(shellune_core::TransferProtocol::Scp),
        "the host's own choice is what the question starts with"
    );
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let starts = transfers.starts();
    assert_eq!(starts.len(), 1, "the copy started: {starts:?}");
    assert_eq!(starts[0].protocol, shellune_core::TransferProtocol::Scp);
    assert!(
        !starts[0].resume.is_some(),
        "SCP starts from the first byte: there is nothing to continue"
    );

    // A finished SCP task offers no resume entry, even when a checkpoint from elsewhere
    // names an offset: the protocol cannot continue one, so it must not promise to.
    let task = starts[0].transfer;
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Failed {
            failure: shellune_core::TransferFailure::ConnectionLost,
            residue: None,
            resume: Some(shellune_core::ResumeHint {
                partial: "/home/deploy/.shellune-part-kept".to_owned(),
                record: "C:/temp/kept.json".to_owned(),
                offset: 4096,
                generation: 1,
            }),
        },
    });
    pump_files(cx, &view);
    assert!(
        cx.debug_bounds("transfers.resume.0").is_none(),
        "an SCP task has no resume button"
    );
    assert!(
        cx.debug_bounds("transfers.retry.0").is_some()
            || cx.debug_bounds("transfers.restart.0").is_some(),
        "it can be copied again from the first byte"
    );

    // And the other protocol is still one task away, not a change to the host.
    click(cx, "files.upload");
    click(cx, "files.question.protocol.sftp");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let starts = transfers.starts();
    assert_eq!(
        starts.last().map(|start| start.protocol),
        Some(shellune_core::TransferProtocol::Sftp),
        "a single transfer may choose the other protocol"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .book()
            .hosts()
            .first()
            .map(HostConfig::transfer_protocol)),
        Some(shellune_core::TransferProtocol::Scp),
        "that choice did not change the saved host"
    );
}

#[gpui::test]
async fn a_server_without_sftp_says_so_and_offers_the_scp_copy_it_still_has(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let files = FileAnswers::accepting();
    let channel = files.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(id, "web", "deploy@10.0.0.5:22", Box::new(channel), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "session.files");
    files.fail(
        1,
        shellune_core::BrowseFailure::Unavailable {
            reason: "the server refused the sftp subsystem".to_owned(),
        },
    );
    pump_files(cx, &view);

    assert!(
        cx.debug_bounds("files.failure.reason").is_some(),
        "the server really has no SFTP, so no listing is invented"
    );
    assert!(
        cx.debug_bounds("files.failure.scp.hint").is_some(),
        "the next step names the protocol that still copies files"
    );
    // Both directions stay reachable without a listing: the toolbar and the card each
    // offer the typed path, because a copy that only works one way is not what this
    // server can still do.
    assert!(
        cx.debug_bounds("files.upload").is_some(),
        "the toolbar still offers the typed upload"
    );
    assert!(
        cx.debug_bounds("files.download").is_some(),
        "the toolbar offers the typed download as well"
    );
    assert!(
        cx.debug_bounds("files.failure.scp.upload").is_some(),
        "the card offers the upload beside the reason"
    );
    assert!(
        cx.debug_bounds("files.failure.scp.download").is_some(),
        "the card offers the download beside the reason"
    );
    // The upload question is still reachable, and it is where the remote path is typed.
    click(cx, "files.upload");
    assert!(cx.debug_bounds("files.question.remote").is_some());
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("~".to_owned()),
        "with no listing the remote directory starts at the account's home"
    );
}

#[gpui::test]
async fn the_scp_path_on_a_failed_listing_is_reached_by_scrolling_the_pane(
    cx: &mut TestAppContext,
) {
    // R4 measures this page in the minimum window at 200% text, where the file pane is
    // one slice tall and the failed card is much taller than that slice. Its next step -
    // the reason and the two typed-path entries - has to be reachable there, or the only
    // way to copy a file from a server without SFTP is drawn past an edge nothing can
    // reach.
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let files = FileAnswers::accepting();
    let channel = files.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(id, "web", "deploy@10.0.0.5:22", Box::new(channel), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "session.files");
    files.fail(
        1,
        shellune_core::BrowseFailure::Unavailable {
            reason: "the server refused the sftp subsystem".to_owned(),
        },
    );
    pump_files(cx, &view);

    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    largest_text_size(&view, cx);
    cx.run_until_parked();

    let viewport = minimum_window();
    // The header's upload and download stay in the pane's own toolbar, so either
    // direction can be started without turning anything.
    for selector in ["files.upload", "files.download"] {
        assert_within_width(bounds_of(cx, selector), viewport, selector);
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    // The reason comes first, then the caption, then the two entries: each is reached by
    // turning the pane down, and once reached it is inside the window rather than under a
    // clipped edge.
    scroll_fully_into_view(cx, "files.panel", "files.failure.reason");
    let reason = bounds_of(cx, "files.failure.reason");
    assert_within_width(reason, viewport, "files.failure.reason");
    assert_fits_inside(reason, viewport, "files.failure.reason");

    // The card is taller than the whole slice, which is why the pane has to turn: the
    // caption's middle comes into the slice and the rest of the card is read by turning
    // on.
    scroll_into_view(cx, "files.panel", "files.failure.scp.hint");
    let hint = bounds_of(cx, "files.failure.scp.hint");
    let panel = bounds_of(cx, "files.panel");
    let card = bounds_of(cx, "files.failure");
    assert_within_width(hint, viewport, "files.failure.scp.hint");
    assert!(
        card.size.height > panel.size.height,
        "the card is taller than the slice, so it is read by turning: {card:?} in {panel:?}"
    );
    let centre = hint.center();
    assert!(
        centre.y >= panel.origin.y - px(0.5)
            && centre.y <= panel.origin.y + panel.size.height + px(0.5),
        "the caption's middle is inside the pane once it is turned: {centre:?} in {panel:?}"
    );

    for selector in ["files.failure.scp.upload", "files.failure.scp.download"] {
        scroll_fully_into_view(cx, "files.panel", selector);
        let bounds = bounds_of(cx, selector);
        assert_within_width(bounds, viewport, selector);
        assert_fits_inside(bounds, viewport, selector);
        assert_pointer_target(cx, selector);
    }
    // The pane is what turned, not a card drawn past an edge: its own header went up with
    // the rest, so nothing in the card stays under the clip.
    let header = bounds_of(cx, "files.header");
    assert!(
        header.origin.y < panel.origin.y,
        "the pane scrolled its header out of the slice: {header:?} in {panel:?}"
    );
}

#[gpui::test]
async fn a_server_without_sftp_downloads_the_remote_path_the_user_types(cx: &mut TestAppContext) {
    // R10 asks for an upload *and* a download on a server that offers no SFTP. With no
    // listing there is no row to start a download from, so the typed remote path is the
    // only way one can exist - and it has to reach the same task a row would.
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let files = FileAnswers::accepting();
    let channel = files.clone();
    let transfers = TransferAnswers::accepting();
    let transfer_channel = transfers.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(id, "web", "deploy@10.0.0.5:22", Box::new(channel), cx);
            view.attach_transfers_for_test(id, Box::new(transfer_channel), cx);
        });
    });
    cx.run_until_parked();
    click(cx, "session.files");
    files.fail(
        1,
        shellune_core::BrowseFailure::Unavailable {
            reason: "the server refused the sftp subsystem".to_owned(),
        },
    );
    pump_files(cx, &view);

    // The card's own download entry opens the question, and the remote path is typed
    // there instead of picked from a list that does not exist.
    click(cx, "files.failure.scp.download");
    assert!(cx.debug_bounds("files.question.remote").is_some());
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("~".to_owned()),
        "the typed path starts at the account's home"
    );
    // SCP is the protocol that can copy here; the choice belongs to this one task.
    click(cx, "files.question.protocol.scp");
    let folder = std::env::temp_dir().join(format!("shellune-scp-download-{}", std::process::id()));
    std::fs::create_dir_all(&folder).expect("a scratch directory can be made");
    let folder = folder.to_string_lossy().into_owned();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_remote_for_test("~/report.txt");
            view.files_question_set_text_for_test(&folder);
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let starts = transfers.starts();
    assert_eq!(
        starts.len(),
        1,
        "the typed download started exactly one task: {starts:?}"
    );
    assert_eq!(starts[0].direction, TransferDirection::Download);
    assert_eq!(starts[0].protocol, shellune_core::TransferProtocol::Scp);
    assert_eq!(
        starts[0].source, "~/report.txt",
        "the remote path is exactly what was typed, so the shell can expand it"
    );
    assert!(
        starts[0]
            .destination
            .ends_with(&format!("{}report.txt", std::path::MAIN_SEPARATOR)),
        "the file lands under its own name in the chosen folder: {}",
        starts[0].destination
    );
    assert!(
        !starts[0].resume.is_some(),
        "an SCP download has nothing to continue"
    );
    let _ = std::fs::remove_dir_all(&folder);

    // And the toolbar entry is the same question, not a second kind of task.
    click(cx, "files.download");
    assert!(cx.debug_bounds("files.question.remote").is_some());
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("~".to_owned())
    );
}

// ---------------------------------------------------------------------------
// The file view's keyboard path (R4, R10)
// ---------------------------------------------------------------------------

/// The keystroke the catalogue binds to one action on this platform.
fn catalog_shortcut(id: &str) -> String {
    Keymap::defaults(Platform::current())
        .keystroke(id)
        .unwrap_or_else(|| panic!("{id} has no shortcut"))
        .to_owned()
}

/// Presses one keystroke on the window, as a keyboard does, and lets the page settle.
fn press_key(cx: &mut VisualTestContext, keystroke: &str) {
    cx.simulate_keystrokes(keystroke);
    cx.run_until_parked();
}

/// The file view's stop that holds the keyboard, for a test that says where it is.
fn files_stop(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
) -> Option<(FileSource, FilesStop)> {
    cx.update(|window, app| view.read(app).files_keyboard_stop(window, app))
}

/// Types into the field that holds the keyboard and lets the page settle.
fn type_here(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    cx.run_until_parked();
}

/// A remote session with scripted files and transfers, for a keyboard test.
fn keyboard_remote(
    cx: &mut TestAppContext,
) -> (
    Entity<ShelluneView>,
    &mut VisualTestContext,
    SessionId,
    FileAnswers,
    TransferAnswers,
) {
    let (view, cx) = remote_view(cx);
    // The shortcuts are what this test exercises, so the catalogue is bound as a window
    // does on start.
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    let id = live_remote(cx, &view);
    let files = FileAnswers::accepting();
    let transfers = TransferAnswers::accepting();
    let file_channel = files.clone();
    let transfer_channel = transfers.clone();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_files_for_test(id, "web", "deploy@10.0.0.5:22", Box::new(file_channel), cx);
            view.attach_transfers_for_test(id, Box::new(transfer_channel), cx);
        });
    });
    cx.run_until_parked();
    (view, cx, id, files, transfers)
}

#[gpui::test]
async fn the_upload_question_is_asked_and_answered_with_the_keyboard_alone(
    cx: &mut TestAppContext,
) {
    // R4 asks for file transfer to be usable without a pointer, and R10 makes that the
    // only way a file moves on a server that refuses the SFTP subsystem. Nothing in this
    // test clicks: the shortcut opens the file view of the session in front and asks about
    // the upload, Tab walks the question, and Enter confirms it.
    let (view, cx, _id, files, transfers) = keyboard_remote(cx);

    press_key(cx, &catalog_shortcut("files.upload"));
    assert!(
        cx.update(|_window, app| view.read(app).files_question_open()),
        "the shortcut asks about the upload"
    );
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionLocal)),
        "the question starts at the local file to send"
    );
    files.fail(
        1,
        shellune_core::BrowseFailure::Unavailable {
            reason: "the server refused the sftp subsystem".to_owned(),
        },
    );
    pump_files(cx, &view);
    assert!(
        cx.debug_bounds("files.failure.scp.upload").is_some(),
        "the read failed, so the typed path is the only way a copy can start"
    );

    // The local file to send, then the remote directory it lands in.
    type_here(cx, "C:/work/report.txt");
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionRemote)),
        "Tab reaches the remote directory"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("~".to_owned()),
        "with no listing the directory starts at the account's home"
    );
    type_here(cx, "/inbox");

    // The protocol belongs to this task, so the switch is part of the same walk.
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionSftp))
    );
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionScp))
    );
    press_key(cx, "enter");
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_protocol()),
        Some(shellune_core::TransferProtocol::Scp),
        "Enter on the SCP choice chooses it for this one task"
    );

    // SCP brought the recursive switch with it; Tab walks past it to the confirmation.
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionRecursive))
    );
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionConfirm))
    );
    press_key(cx, "enter");

    let starts = transfers.starts();
    assert_eq!(starts.len(), 1, "the question started one task: {starts:?}");
    assert_eq!(starts[0].direction, TransferDirection::Upload);
    assert_eq!(starts[0].protocol, shellune_core::TransferProtocol::Scp);
    assert_eq!(starts[0].source, "C:/work/report.txt");
    assert_eq!(
        starts[0].destination, "~/inbox/report.txt",
        "the directory that was typed is exactly the one used"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).files_question_open()),
        "the question is answered and gone"
    );
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::Body)),
        "the column takes the keyboard back once its question is answered"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).pointer_input_for_test()),
        "no click happened anywhere in this flow"
    );
}

#[gpui::test]
async fn the_download_question_is_asked_and_answered_with_the_keyboard_alone(
    cx: &mut TestAppContext,
) {
    // The same walk in the other direction: the shortcut asks which remote path is
    // downloaded, and the copy lands in the folder the question proposed.
    let (view, cx, _id, files, transfers) = keyboard_remote(cx);

    press_key(cx, &catalog_shortcut("files.download"));
    assert!(
        cx.update(|_window, app| view.read(app).files_question_open()),
        "the shortcut asks about the download"
    );
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionLocal)),
        "the question starts at the folder the file lands in"
    );
    let folder = cx
        .update(|_window, app| view.read(app).files_question_text().map(str::to_owned))
        .expect("the folder the copy lands in is proposed");
    assert!(!folder.trim().is_empty(), "a real folder is proposed");
    files.fail(
        1,
        shellune_core::BrowseFailure::Unavailable {
            reason: "the server refused the sftp subsystem".to_owned(),
        },
    );
    pump_files(cx, &view);

    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionRemote))
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("~".to_owned())
    );
    type_here(cx, "/report.txt");
    press_key(cx, "tab");
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionScp))
    );
    press_key(cx, "enter");
    press_key(cx, "tab");
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionConfirm))
    );
    press_key(cx, "enter");

    let starts = transfers.starts();
    assert_eq!(starts.len(), 1, "the question started one task: {starts:?}");
    assert_eq!(starts[0].direction, TransferDirection::Download);
    assert_eq!(starts[0].protocol, shellune_core::TransferProtocol::Scp);
    assert_eq!(starts[0].source, "~/report.txt");
    assert!(
        starts[0]
            .destination
            .ends_with(&format!("{}report.txt", std::path::MAIN_SEPARATOR)),
        "the file lands under its own name in the proposed folder: {}",
        starts[0].destination
    );
    assert!(
        !cx.update(|_window, app| view.read(app).pointer_input_for_test()),
        "no click happened anywhere in this flow"
    );
}

#[gpui::test]
async fn the_failed_reads_typed_entries_are_reached_and_pressed_with_the_keyboard(
    cx: &mut TestAppContext,
) {
    // The two typed-path entries of a failed read are drawn below the slice at 200% text in
    // the minimum window, and they are the only way a file is copied from a server without
    // SFTP. Page Up/Down turn the pane and Tab walks the column's own controls, so the
    // whole path exists without a pointer (R4).
    let (view, cx, _id, files, transfers) = keyboard_remote(cx);

    // The page is already as small and as large-typed as it gets, so the slice the entries
    // have to be reached in is the one measured below.
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    largest_text_size(&view, cx);
    cx.run_until_parked();

    // The shortcut opens the file view and asks about the upload; Escape leaves the
    // question, which puts the keyboard in the server column.
    press_key(cx, &catalog_shortcut("files.upload"));
    assert!(cx.update(|_window, app| view.read(app).files_question_open()));
    files.fail(
        1,
        shellune_core::BrowseFailure::Unavailable {
            reason: "the server refused the sftp subsystem".to_owned(),
        },
    );
    pump_files(cx, &view);
    press_key(cx, "escape");
    assert!(
        !cx.update(|_window, app| view.read(app).files_question_open()),
        "Escape closes the question"
    );
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::Body)),
        "the column takes the keyboard back"
    );

    // The question above turned the pane to its own field while it was open, so the pane
    // starts from its top before the card is measured.
    press_key(cx, "home");

    // The pane is one slice tall and the card is taller than that, so the entry starts
    // under an edge nothing can reach without turning the pane.
    let panel = bounds_of(cx, "files.panel");
    let entry = bounds_of(cx, "files.failure.scp.upload");
    assert!(
        entry.origin.y > panel.origin.y + panel.size.height,
        "the typed entry starts below the slice: {entry:?} in {panel:?}"
    );

    // Page Down turns the pane from the keyboard and Page Up brings it back, so the reason
    // above the entries is read as well as the entries themselves.
    let header = bounds_of(cx, "files.header");
    press_key(cx, "pagedown");
    assert!(
        bounds_of(cx, "files.header").origin.y < header.origin.y,
        "Page Down turns the pane, header and all"
    );
    press_key(cx, "pageup");
    assert!(
        (bounds_of(cx, "files.header").origin.y - header.origin.y).abs() < px(0.5),
        "Page Up goes back to the top of the pane"
    );
    press_key(cx, "home");
    assert!(
        (bounds_of(cx, "files.header").origin.y - header.origin.y).abs() < px(0.5),
        "Home is the top of the pane as well"
    );
    press_key(cx, "end");
    let panel = bounds_of(cx, "files.panel");
    let entry = bounds_of(cx, "files.failure.scp.upload");
    assert!(
        entry.origin.y >= panel.origin.y - px(0.5)
            && entry.origin.y + entry.size.height <= panel.origin.y + panel.size.height + px(0.5),
        "End turns to the entries, which are the card's last rows: {entry:?} in {panel:?}"
    );

    // Tab walks the column: its toolbar's two entries, then the failed read's two.
    for expected in [
        FilesStop::ToolbarUpload,
        FilesStop::ToolbarDownload,
        FilesStop::CardUpload,
        FilesStop::CardDownload,
    ] {
        press_key(cx, "tab");
        assert_eq!(
            files_stop(cx, &view),
            Some((FileSource::Remote, expected)),
            "Tab reaches {expected:?}"
        );
    }
    // The arrows step the same ring while a control has it, one step each way.
    press_key(cx, "up");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::CardUpload)),
        "Up steps back one control"
    );
    press_key(cx, "down");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::CardDownload)),
        "Down steps forward again"
    );

    // Both entries are activated from here, and both open the question they stand for.
    press_key(cx, "enter");
    assert!(
        cx.update(|_window, app| view.read(app).files_question_open()),
        "Enter on the download entry asks about the download"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("~".to_owned()),
        "the typed remote path starts at the account's home"
    );
    press_key(cx, "escape");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::Body)),
        "Escape gives the keyboard back to the column"
    );
    for _ in 0..3 {
        press_key(cx, "tab");
    }
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::CardUpload)),
        "the upload entry is three steps from the listing"
    );

    // The ring is on a control the user can see: the pane scrolled the entry into it.
    let panel = bounds_of(cx, "files.panel");
    let entry = bounds_of(cx, "files.failure.scp.upload");
    assert!(
        entry.origin.y >= panel.origin.y - px(0.5)
            && entry.origin.y + entry.size.height <= panel.origin.y + panel.size.height + px(0.5),
        "the ring is drawn on an entry inside the slice: {entry:?} in {panel:?}"
    );

    // Enter on the entry asks the same question the pointer's press asks, and the keyboard
    // answers it: the copy starts without a click anywhere.
    press_key(cx, "enter");
    assert!(
        cx.update(|_window, app| view.read(app).files_question_open()),
        "Enter on the entry asks about the upload"
    );
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionLocal))
    );
    type_here(cx, "C:/work/report.txt");
    press_key(cx, "tab");
    press_key(cx, "tab");
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionScp))
    );
    press_key(cx, "enter");
    press_key(cx, "tab");
    press_key(cx, "tab");
    press_key(cx, "enter");
    let starts = transfers.starts();
    assert_eq!(
        starts.len(),
        1,
        "the typed entry started one task: {starts:?}"
    );
    assert_eq!(starts[0].direction, TransferDirection::Upload);
    assert_eq!(starts[0].protocol, shellune_core::TransferProtocol::Scp);
    assert!(
        !cx.update(|_window, app| view.read(app).pointer_input_for_test()),
        "no click happened anywhere in this flow"
    );
}

/// Whether a control lies inside the slice a column shows (R4).
///
/// The ring is drawn on the control itself, so "the keyboard is here" is only true if the
/// control is inside the box the column shows: a control under an edge is a place the
/// person cannot see. The bounds are painted boxes, so this measures what is on screen
/// rather than what the column's offset happens to be.
fn assert_inside_slice(inner: gpui::Bounds<Pixels>, outer: gpui::Bounds<Pixels>, what: &str) {
    assert!(
        inner.size.height > px(0.0),
        "{what} was measured with an empty box: {inner:?}"
    );
    assert!(
        inner.origin.y >= outer.origin.y - px(0.5)
            && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + px(0.5),
        "{what} is under an edge of the column: {inner:?} in {outer:?}"
    );
}

#[gpui::test]
async fn every_control_the_keyboard_stops_on_is_turned_into_its_column(cx: &mut TestAppContext) {
    // R4 asks for a ring the user can see, and the ring is drawn on the control itself. At
    // 200% text in the minimum window the question is much taller than the slice a column
    // shows, so every stop has to turn the column to its own row - not only the failed
    // card's two typed entries, which are the card's last rows.
    let (view, cx, _id, files, _transfers) = keyboard_remote(cx);
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    largest_text_size(&view, cx);
    cx.run_until_parked();

    // The shortcut opens the question with the keyboard in its first field, which is the
    // first stop of the walk.
    press_key(cx, &catalog_shortcut("files.upload"));
    files.fail(
        1,
        shellune_core::BrowseFailure::Unavailable {
            reason: "the server refused the sftp subsystem".to_owned(),
        },
    );
    pump_files(cx, &view);
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionLocal)),
        "the question opens with the keyboard in its local path"
    );
    let panel = bounds_of(cx, "files.panel");
    assert_inside_slice(
        bounds_of(cx, "files.question.input"),
        panel,
        "the field the question opens at",
    );

    // The rest of the question: the remote path, the protocol, and the two buttons at its
    // end. Each one is turned into the slice as the keyboard reaches it.
    for (steps, selector) in [
        (1, "files.question.remote"),
        (2, "files.question.protocol"),
        (1, "files.question.confirm"),
        (1, "files.question.cancel"),
    ] {
        for _ in 0..steps {
            press_key(cx, "tab");
        }
        let panel = bounds_of(cx, "files.panel");
        assert_inside_slice(bounds_of(cx, selector), panel, selector);
    }
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::QuestionCancel)),
        "the cancelling button is the last stop of the question"
    );

    // The toolbar sits at the top of the column. With the pane turned to the card's end,
    // Tab back to the upload entry turns it to the toolbar, so the ring is drawn there and
    // not above the slice.
    press_key(cx, "escape");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::Body))
    );
    press_key(cx, "end");
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::ToolbarUpload))
    );
    let panel = bounds_of(cx, "files.panel");
    assert_inside_slice(bounds_of(cx, "files.upload"), panel, "the toolbar's upload");
    press_key(cx, "tab");
    assert_eq!(
        files_stop(cx, &view),
        Some((FileSource::Remote, FilesStop::ToolbarDownload))
    );
    let panel = bounds_of(cx, "files.panel");
    assert_inside_slice(
        bounds_of(cx, "files.download"),
        panel,
        "the toolbar's download",
    );
    assert!(
        !cx.update(|_window, app| view.read(app).pointer_input_for_test()),
        "no click happened anywhere in this flow"
    );
}

#[gpui::test]
async fn the_file_view_is_left_with_the_keyboard_and_the_terminal_takes_the_keys_back(
    cx: &mut TestAppContext,
) {
    // The file view takes the terminal's place while it is open, so without an exit the
    // keyboard would have no way back to the program (R4). Two exits are on the keyboard:
    // the switch back to the terminal in the session bar, which is a stop of the column's
    // own walk, and Escape, which gives up one layer at a time.
    let (view, cx, id, _files, _transfers) = keyboard_remote(cx);

    press_key(cx, &catalog_shortcut("files.upload"));
    assert!(
        cx.update(|_window, app| view.read(app).files_question_open()),
        "the shortcut asks about the upload"
    );
    type_here(cx, "C:/work/report.txt");
    press_key(cx, "escape");
    assert!(
        !cx.update(|_window, app| view.read(app).files_question_open()),
        "Escape gives up the question first"
    );
    assert!(
        cx.update(|_window, app| view.read(app).files_visible(id)),
        "the view itself is still open"
    );

    // The last stop of the walk is the session bar's switch back to the terminal: Tab
    // reaches it from the column, and Enter presses it. How many steps that takes depends
    // on what the column holds - a read that answered has a listing and no download entry,
    // a read still in flight has neither - so the walk goes until the switch.
    let mut steps = 0;
    while files_stop(cx, &view) != Some((FileSource::Remote, FilesStop::Close)) && steps < 8 {
        press_key(cx, "tab");
        steps += 1;
    }
    let at = files_stop(cx, &view);
    assert_eq!(
        at,
        Some((FileSource::Remote, FilesStop::Close)),
        "the way back to the terminal is the last stop of the column, at {at:?}"
    );
    let bar = bounds_of(cx, "files.close");
    assert!(
        bar.size.height > px(0.0),
        "the switch that holds the ring is drawn: {bar:?}"
    );
    press_key(cx, "enter");
    assert!(
        !cx.update(|_window, app| view.read(app).files_visible(id)),
        "Enter on the switch closes the file view"
    );
    assert_eq!(
        files_stop(cx, &view),
        None,
        "no control of the file view holds the keyboard any more"
    );
    assert!(
        cx.update(|window, app| view.read(app).terminal_has_the_keyboard(window, app)),
        "the keys go back to the program in the terminal"
    );

    // The same view opened again and left with Escape: the question goes first, the view
    // second, and the terminal takes the keys back either way.
    press_key(cx, &catalog_shortcut("files.upload"));
    assert!(
        cx.update(|_window, app| view.read(app).files_visible(id)),
        "the shortcut opens the file view again"
    );
    press_key(cx, "escape");
    press_key(cx, "escape");
    assert!(
        !cx.update(|_window, app| view.read(app).files_visible(id)),
        "Escape closes the file view and the terminal takes its place back"
    );
    assert_eq!(
        files_stop(cx, &view),
        None,
        "no control of the file view holds the keyboard any more"
    );
    assert!(
        cx.update(|window, app| view.read(app).terminal_has_the_keyboard(window, app)),
        "the keys go back to the program in the terminal"
    );
    assert_eq!(
        destination_of(&view, cx),
        Destination::LocalShells,
        "the terminal page is the one on screen"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).pointer_input_for_test()),
        "no click happened anywhere in this flow"
    );
}

#[gpui::test]
async fn a_directory_over_sftp_names_the_protocol_that_can_copy_it(cx: &mut TestAppContext) {
    // A directory is not "a path without a name": SFTP copies one file, and the step the
    // user has to take is the other protocol plus the recursive choice. The reason has
    // to say that, or the field marks a path that is perfectly well written.
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_protocol()),
        Some(shellune_core::TransferProtocol::Sftp),
        "the question starts at the host's protocol"
    );
    let directory = std::env::temp_dir().join(format!("shellune-sftp-tree-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a scratch directory can be made");
    let directory = directory.to_string_lossy().into_owned();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test(&directory);
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        transfers.starts().is_empty(),
        "SFTP copies one file, so nothing was started"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_text().map(str::to_owned)),
        Some(directory.clone()),
        "the path that was refused stays in the field"
    );
    assert!(
        cx.debug_bounds("files.question.problem").is_some(),
        "the question says why it cannot start"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_path_problem()),
        Some(crate::app::transfers::PathProblem::DirectoryNeedsScp),
        "a directory over SFTP names the protocol that can copy it, not a missing name"
    );
    let reason = crate::app::copy::Locale::English
        .strings()
        .transfers_problem_directory_needs_scp;
    assert!(
        reason.contains("SCP"),
        "the reason names the protocol that can copy a directory: {reason}"
    );
    assert!(
        !reason.contains("no file"),
        "a directory is not a path without a name: {reason}"
    );

    // The next step really works: SCP with the recursive choice copies the tree.
    click(cx, "files.question.protocol.scp");
    click(cx, "files.question.recursive");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test(&directory);
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let starts = transfers.starts();
    assert_eq!(starts.len(), 1, "the chosen copy started: {starts:?}");
    assert_eq!(starts[0].protocol, shellune_core::TransferProtocol::Scp);
    assert!(starts[0].recursive);
    let _ = std::fs::remove_dir_all(&directory);
}

#[gpui::test]
async fn a_download_whose_total_is_unknown_shows_bytes_and_no_percentage(cx: &mut TestAppContext) {
    // A recursive SCP download cannot know the size of the tree while it arrives. What
    // it reports is bytes: a percentage beside a total that is not the tree would be a
    // number smaller than what was already written, and a share of the wrong whole is
    // not progress.
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.download.1");
    assert!(cx.debug_bounds("files.question.remote").is_some());
    assert_eq!(
        cx.update(|_window, app| view.read(app).files_question_remote().map(str::to_owned)),
        Some("/home/deploy/notes.txt".to_owned()),
        "a row asks about the entry it names"
    );
    click(cx, "files.question.confirm");
    let starts = transfers.starts();
    assert_eq!(starts.len(), 1, "the download started: {starts:?}");
    let task = starts[0].transfer;

    transfers.push(shellune_core::TransferEvent::progress(task, 12, None));
    pump_files(cx, &view);
    let detail = cx
        .update(|_window, app| view.read(app).transfer_detail_for_test(task))
        .expect("the task is listed");
    assert!(
        detail.contains("12 B"),
        "the bytes that arrived are shown: {detail}"
    );
    assert!(
        !detail.contains('%'),
        "no percentage is claimed for an unknown total: {detail}"
    );
}

#[gpui::test]
async fn an_existing_target_is_asked_about_and_keeping_it_writes_nothing(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/notes.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;

    // The worker found the target already there.
    transfers.push(shellune_core::TransferEvent::target_exists(
        task,
        Some(2048),
    ));
    pump_files(cx, &view);
    assert!(
        cx.debug_bounds("transfers.overwrite.0").is_some(),
        "the question names the target before anything is written"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).transfer_tasks()[0].state().is_success()),
        "a target that exists is not a finished transfer"
    );

    // Keeping it sends the decision and ends as cancelled, never as complete.
    click(cx, "transfers.overwrite.keep.0");
    let decisions = transfers.decisions();
    assert_eq!(
        decisions,
        vec![(task, shellune_core::OverwriteChoice::KeepExisting)]
    );
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Cancelled {
            residue: None,
            resume: None,
        },
    });
    pump_files(cx, &view);
    let tasks = cx.update(|_window, app| view.read(app).transfer_tasks());
    assert!(!tasks[0].state().is_success());
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        0
    );
    assert!(
        !cx.update(|_window, app| view.read(app).transfer_tasks()[0]
            .state()
            .is_awaiting_overwrite()),
        "an answered question is not asked again"
    );
}

#[gpui::test]
async fn replacing_an_existing_target_is_what_the_user_confirmed(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/notes.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;
    transfers.push(shellune_core::TransferEvent::target_exists(task, Some(16)));
    pump_files(cx, &view);

    click(cx, "transfers.overwrite.replace.0");
    assert_eq!(
        transfers.decisions(),
        vec![(task, shellune_core::OverwriteChoice::Replace)]
    );
    assert!(
        cx.update(|_window, app| view.read(app).transfer_tasks()[0]
            .state()
            .is_awaiting_overwrite()),
        "the transfer only starts writing once the answer is applied"
    );

    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::OverwriteConfirmed,
    });
    transfers.push(shellune_core::TransferEvent::progress(task, 16, Some(16)));
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Completed { bytes: 16 },
    });
    pump_files(cx, &view);
    assert!(
        cx.update(|_window, app| view.read(app).transfer_tasks()[0].state().is_success()),
        "the target really holds the content now"
    );
    // The transfers page reads the same list.
    go(cx, crate::app::actions::GoToTransfers);
    assert!(
        cx.debug_bounds("row.0").is_some(),
        "the finished transfer is listed on its own page"
    );
}

#[gpui::test]
async fn cancelling_a_transfer_stops_it_and_a_retry_is_a_new_task(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;
    transfers.push(shellune_core::TransferEvent::progress(
        task,
        1024,
        Some(4096),
    ));
    pump_files(cx, &view);

    click(cx, "transfers.cancel.0");
    assert_eq!(transfers.cancels(), vec![task]);
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        0,
        "a cancelled task is finished at once"
    );

    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Cancelled {
            residue: Some("/home/deploy/.shellune-part-1".to_owned()),
            resume: None,
        },
    });
    pump_files(cx, &view);
    let tasks = cx.update(|_window, app| view.read(app).transfer_tasks());
    assert_eq!(
        tasks[0].state().residue(),
        Some("/home/deploy/.shellune-part-1"),
        "a partial file that stayed behind is named"
    );

    // Retrying is a second task, not a continuation of the first.
    click(cx, "transfers.retry.0");
    let starts = transfers.starts();
    assert_eq!(starts.len(), 2, "a retry starts a task of its own");
    assert_ne!(starts[1].transfer, task);
    assert_eq!(starts[1].source, "C:/work/report.txt");
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_tasks().len()),
        2,
        "the cancelled task stays readable beside the new one"
    );
}

#[gpui::test]
async fn closing_a_session_with_a_running_transfer_warns_first_and_cancels_only_that_session(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let first = live_remote(cx, &view);
    let (_files, first_transfers) = open_transfers(cx, &view, first);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/a.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = first_transfers.starts()[0].transfer;
    let hint = shellune_core::ResumeHint {
        partial: "/home/deploy/.shellune-part-kept".to_owned(),
        record: "C:/temp/kept.json".to_owned(),
        offset: 2048,
        generation: 1,
    };
    first_transfers.push(shellune_core::TransferEvent::progress(
        task,
        2048,
        Some(8192),
    ));
    first_transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Checkpoint(hint.clone()),
    });
    pump_files(cx, &view);

    // A second session with a transfer of its own, which must survive.
    let second = live_remote_second(cx, &view, first);
    let (_files, second_transfers) = open_transfers(cx, &view, second);
    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/b.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();

    // Closing the first session asks before it cancels anything.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.request_close_session(first, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session.close.question").is_some(),
        "the effect on the transfers is stated first"
    );
    assert!(
        cx.debug_bounds("session.close.task.0").is_some(),
        "the task that would stop is listed, not only counted"
    );
    assert!(
        cx.debug_bounds("session.close.task.1").is_none(),
        "the other session's task is not in this list"
    );
    let listed = cx
        .update(|_window, app| view.read(app).interrupt_line_for_test(task))
        .expect("the task is still known");
    assert!(
        listed.contains("C:/work/a.txt"),
        "the warning names the source: {listed}"
    );
    assert!(
        listed.contains("/home/deploy/a.txt"),
        "the warning names the target: {listed}"
    );
    assert!(
        first_transfers.cancels().is_empty(),
        "asking is not cancelling"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(first)),
        1
    );

    // Cancelling the close leaves everything running.
    click(cx, "session.close.cancel");
    assert_eq!(
        cx.update(|_window, app| view.read(app).closing_for_test()),
        None,
        "the question goes away and nothing is closed"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(first)),
        1,
        "the session and its transfer are untouched"
    );

    // Confirming closes the session, cancels its own task and leaves the other one alone.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.request_close_session(first, cx));
    });
    cx.run_until_parked();
    click(cx, "session.close.confirm");
    assert_eq!(first_transfers.cancels(), vec![task]);
    assert!(
        second_transfers.cancels().is_empty(),
        "the other session's transfer keeps running"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(second)),
        1
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .session(first)
            .map(|session| matches!(session.state(), SessionState::Closed))),
        Some(true),
        "the session really closed"
    );
    let closed = cx.update(|_window, app| {
        view.read(app)
            .transfer_tasks()
            .into_iter()
            .find(|listed| listed.id() == task)
            .expect("the interrupted task stays in the list")
            .state()
            .clone()
    });
    assert!(!closed.is_success());
    assert_eq!(
        closed.resume().map(|item| item.offset),
        Some(2048),
        "the checkpoint already reported is kept when the session goes"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(second)),
        1
    );
}

#[gpui::test]
async fn a_connection_that_ends_mid_transfer_names_the_partial_file_it_left(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/报告 终稿.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let started = transfers.starts();
    let task = started[0].transfer;
    let destination = started[0].destination.clone();
    transfers.push(shellune_core::TransferEvent::progress(
        task,
        1024,
        Some(4096),
    ));
    pump_files(cx, &view);
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        1,
        "the task is moving bytes"
    );

    // The connection ends while the bytes are moving, exactly as the transport reports
    // it. The task's own worker dies with it and can never report the file it was
    // writing, so the window has to name it.
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the remote terminal exists");
    let tag = cx.update(|_window, app| pane.read(app).tag());
    let accepted = cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.adopt_events(
                tag,
                vec![SourceEvent::Lost("the channel was closed".to_owned())],
                cx,
            )
        })
    });
    assert!(accepted, "the event belongs to this session");
    cx.run_until_parked();

    let tasks = cx.update(|_window, app| view.read(app).transfer_tasks());
    let failed = tasks
        .iter()
        .find(|listed| listed.id() == task)
        .expect("the task stays readable after its session ended");
    assert!(
        failed.state().failure().is_some(),
        "a lost connection is a failure, not a transfer that keeps running: {:?}",
        failed.state()
    );
    let residue = failed
        .state()
        .residue()
        .expect("the partial file the task was writing is named");
    let parent = destination
        .rsplit_once('/')
        .map(|(directory, _name)| directory)
        .expect("the target is inside a directory");
    assert_eq!(
        residue,
        format!("{parent}/{}{task}", shellune_core::PARTIAL_PREFIX),
        "the residue is the temporary file the worker was writing beside the target"
    );

    // The row the user reads carries it: the file view is closed with the session, so
    // the transfers page is where a task of a gone session is still shown.
    let detail = cx
        .update(|_window, app| view.read(app).transfer_detail_for_test(task))
        .expect("the task is listed");
    assert!(
        detail.contains(residue),
        "the panel has to show the partial file, not hide it: {detail:?}"
    );
    assert!(
        detail.contains(ENGLISH.transfers_residue),
        "and it has to say what the path is: {detail:?}"
    );
}

#[gpui::test]
async fn a_resumed_transfer_keeps_its_partial_when_the_connection_ends_and_a_later_offset(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let started = transfers.starts();
    let task = started[0].transfer;
    let destination = started[0].destination.clone();
    let stale = shellune_core::ResumeHint {
        partial: "/home/deploy/.shellune-part-original".to_owned(),
        record: "C:/temp/original.json".to_owned(),
        offset: 1024,
        generation: 1,
    };
    transfers.push(shellune_core::TransferEvent::progress(
        task,
        1024,
        Some(8192),
    ));
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Checkpoint(stale.clone()),
    });
    pump_files(cx, &view);

    let pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the remote terminal exists");
    let tag = cx.update(|_window, app| pane.read(app).tag());
    let accepted = cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.adopt_events(
                tag,
                vec![SourceEvent::Lost("the channel was closed".to_owned())],
                cx,
            )
        })
    });
    assert!(accepted, "the event belongs to this session");
    cx.run_until_parked();

    let invented = {
        let parent = destination
            .rsplit_once('/')
            .map(|(directory, _name)| directory)
            .expect("the target is inside a directory");
        format!("{parent}/{}{task}", shellune_core::PARTIAL_PREFIX)
    };
    let detail = cx
        .update(|_window, app| view.read(app).transfer_detail_for_test(task))
        .expect("the task is listed");
    assert!(
        detail.contains(&stale.partial),
        "the residue is the partial file the resume was writing: {detail}"
    );
    assert!(
        !detail.contains(&invented),
        "the list must not name a partial file this attempt never created: {detail}"
    );
    assert!(
        detail.contains("1024"),
        "the frozen checkpoint is still shown: {detail}"
    );

    let flushed = shellune_core::ResumeHint {
        offset: 4096,
        ..stale.clone()
    };
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Failed {
            failure: shellune_core::TransferFailure::ConnectionLost,
            residue: Some(flushed.partial.clone()),
            resume: Some(flushed.clone()),
        },
    });
    pump_files(cx, &view);
    let tasks = cx.update(|_window, app| view.read(app).transfer_tasks());
    let failed = tasks
        .iter()
        .find(|listed| listed.id() == task)
        .expect("the task stays listed");
    assert!(!failed.state().is_success());
    assert_eq!(failed.state().resume(), Some(&flushed));
    assert_eq!(failed.state().residue(), Some(flushed.partial.as_str()));
    let detail = cx
        .update(|_window, app| view.read(app).transfer_detail_for_test(task))
        .expect("the task is listed");
    assert!(
        detail.contains("4096"),
        "the later flushed offset replaces the stale checkpoint: {detail}"
    );
    assert!(detail.contains(&flushed.partial), "{detail}");
}

#[gpui::test]
async fn a_delete_the_browser_refuses_keeps_its_question_and_says_why(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let files = open_files(cx, &view, id);

    click(cx, "files.delete.1");
    assert!(
        cx.debug_bounds("files.question").is_some(),
        "the delete question is open"
    );

    // The entry is gone from the directory: refreshing is what makes the confirmation
    // stale, exactly as another person deleting it would.
    click(cx, "files.refresh");
    files.reply(
        2,
        "/home/deploy",
        &[("docs", shellune_core::FileKind::Directory)],
    );
    pump_files(cx, &view);
    click(cx, "files.question.confirm");
    assert!(
        cx.debug_bounds("files.question.problem").is_some(),
        "a confirmation that cannot be sent states the reason instead of doing nothing"
    );
    assert!(
        cx.debug_bounds("files.question").is_some(),
        "the question stays open with the reason beside it"
    );
}

#[gpui::test]
async fn a_connection_that_cannot_take_a_transfer_says_so(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let _files = open_files(cx, &view, id);
    // A channel whose connection is already gone.
    let closed = TransferAnswers::default();
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.attach_transfers_for_test(id, Box::new(closed.clone()), cx);
        });
    });
    cx.run_until_parked();

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/a.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("transfers.refused").is_some(),
        "a command that never left the process is reported where the list is"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        0,
        "no task is left claiming to run"
    );
}

#[gpui::test]
async fn the_transfer_center_keeps_a_task_after_the_file_view_closes(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);
    let host = cx.update(|_window, app| {
        view.read(app)
            .workspace()
            .session(id)
            .expect("the session is listed")
            .title()
            .to_owned()
    });

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/报告 终稿.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;
    transfers.push(shellune_core::TransferEvent::progress(
        task,
        2048,
        Some(8192),
    ));
    pump_files(cx, &view);

    click(cx, "files.close");
    assert!(
        !cx.update(|_window, app| view.read(app).files_visible(id)),
        "the file view is closed"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        1,
        "closing the file view does not stop the transfer"
    );
    go(cx, crate::app::actions::GoToTransfers);
    assert_eq!(
        cx.update(|_window, app| view.read(app).tool_for_test()),
        Some(crate::app::view::Tool::Transfers)
    );
    assert!(
        cx.debug_bounds("transfers.center.summary.0").is_some(),
        "the same task is on the transfer center"
    );
    let summary = cx
        .update(|_window, app| view.read(app).transfer_center_text_for_test(task))
        .expect("the center reads the task");
    assert!(summary.contains(&host), "the host is named: {summary}");
    assert!(
        summary.contains("C:/work/报告 终稿.txt"),
        "the source is named: {summary}"
    );
    assert!(
        summary.contains("/home/deploy/报告 终稿.txt"),
        "the target is named: {summary}"
    );
    assert!(
        summary.contains("2048/8192"),
        "the progress is the worker's number: {summary}"
    );

    // A later worker event still reaches the center after the file view is gone.
    transfers.push(shellune_core::TransferEvent::progress(
        task,
        4096,
        Some(8192),
    ));
    pump_files(cx, &view);
    let later = cx
        .update(|_window, app| view.read(app).transfer_center_text_for_test(task))
        .expect("the center still has the task");
    assert!(
        later.contains("4096/8192"),
        "the center follows the worker, not the number it had when the files closed: {later}"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).transfer_tasks()[0].state().is_success()),
        "progress is not completion"
    );

    click(cx, "transfers.center.cancel.0");
    assert_eq!(transfers.cancels(), vec![task]);
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        0
    );
}

#[gpui::test]
async fn an_overwrite_answer_from_the_transfer_center_is_the_same_decision(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/notes.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;
    transfers.push(shellune_core::TransferEvent::target_exists(task, Some(32)));
    pump_files(cx, &view);

    click(cx, "files.close");
    assert!(
        !cx.update(|_window, app| view.read(app).files_visible(id)),
        "the file view is closed"
    );
    go(cx, crate::app::actions::GoToTransfers);
    assert!(
        cx.debug_bounds("transfers.center.overwrite.keep.0")
            .is_some(),
        "the center asks before anything is written"
    );
    assert!(!cx.update(|_window, app| view.read(app).transfer_tasks()[0].state().is_success()),);

    click(cx, "transfers.center.overwrite.keep.0");
    assert_eq!(
        transfers.decisions(),
        vec![(task, shellune_core::OverwriteChoice::KeepExisting)]
    );
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Cancelled {
            residue: None,
            resume: None,
        },
    });
    pump_files(cx, &view);
    assert!(
        !cx.update(|_window, app| view.read(app).transfer_tasks()[0].state().is_success()),
        "keeping the file is not a completed transfer"
    );
}

#[gpui::test]
async fn resume_carries_the_checkpoint_and_transfer_again_drops_it(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;
    let hint = shellune_core::ResumeHint {
        partial: "/home/deploy/.shellune-part-kept".to_owned(),
        record: "C:/temp/kept.json".to_owned(),
        offset: 2048,
        generation: 1,
    };
    transfers.push(shellune_core::TransferEvent::progress(
        task,
        2048,
        Some(8192),
    ));
    pump_files(cx, &view);
    click(cx, "transfers.cancel.0");
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Cancelled {
            residue: Some(hint.partial.clone()),
            resume: Some(hint.clone()),
        },
    });
    pump_files(cx, &view);
    let detail = cx
        .update(|_window, app| view.read(app).transfer_detail_for_test(task))
        .expect("the cancelled task is listed");
    assert!(
        detail.contains("2048"),
        "the confirmed offset is shown as a checkpoint, not as a finished file: {detail}"
    );
    assert!(!cx.update(|_window, app| view.read(app).transfer_tasks()[0].state().is_success()),);

    click(cx, "transfers.resume.0");
    let starts = transfers.starts();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1].resume.as_ref(), Some(&hint));
    assert_ne!(starts[1].transfer, task);
    let resumed = starts[1].transfer;
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .transfer_tasks()
                .into_iter()
                .find(|listed| listed.id() == resumed)
                .expect("the new task is listed")
                .state()
                .clone()
        }),
        shellune_core::TransferState::Queued,
        "the old offset is not progress of the new task"
    );
    assert!(
        cx.update(|_window, app| {
            view.read(app)
                .transfer_tasks()
                .iter()
                .any(|listed| listed.id() == task && !listed.state().is_success())
        }),
        "the cancelled task stays beside the new one"
    );

    click(cx, "transfers.restart.0");
    let starts = transfers.starts();
    assert!(
        starts
            .last()
            .is_some_and(|request| request.resume.is_none()),
        "transfer again starts at the first byte: {starts:?}"
    );
    assert_ne!(starts.last().map(|request| request.transfer), Some(task));
    assert_ne!(starts.last().map(|request| request.transfer), Some(resumed));
}

#[gpui::test]
async fn a_refused_resume_stays_failed_and_transfer_again_is_a_new_task(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;
    transfers.push(shellune_core::TransferEvent::progress(task, 100, Some(400)));
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Failed {
            failure: shellune_core::TransferFailure::ResumeRefused {
                reason: shellune_core::ResumeRefusal::SourceChanged,
            },
            residue: None,
            resume: None,
        },
    });
    pump_files(cx, &view);
    let detail = cx
        .update(|_window, app| view.read(app).transfer_detail_for_test(task))
        .expect("the refusal is listed");
    assert!(
        detail.contains(ENGLISH.transfers_resume_source),
        "the refusal says the source changed: {detail}"
    );
    assert!(
        !detail.contains("100/400"),
        "a refused resume does not keep the old progress as if it still counted: {detail}"
    );

    click(cx, "files.close");
    go(cx, crate::app::actions::GoToTransfers);
    assert!(cx.debug_bounds("transfers.center.restart.0").is_some());
    click(cx, "transfers.center.restart.0");
    let starts = transfers.starts();
    assert_eq!(starts.len(), 2);
    assert!(starts[1].resume.is_none());
    assert_ne!(starts[1].transfer, task);
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_tasks().len()),
        2,
        "the refusal stays in the list"
    );
    assert!(cx.update(|_window, app| {
        view.read(app)
            .transfer_tasks()
            .iter()
            .any(|listed| listed.id() == task && !listed.state().is_success())
    }),);
}

#[gpui::test]
async fn closing_the_application_lists_the_transfers_and_stopping_is_confirmed(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let first = live_remote(cx, &view);
    let (_files, first_transfers) = open_transfers(cx, &view, first);
    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/a.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let first_task = first_transfers.starts()[0].transfer;

    let second = live_remote_second(cx, &view, first);
    let (_files, second_transfers) = open_transfers(cx, &view, second);
    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/b.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let second_task = second_transfers.starts()[0].transfer;

    assert!(
        !cx.simulate_close(),
        "a window with running transfers does not close on the first request"
    );
    cx.run_until_parked();
    assert!(cx.update(|_window, app| view.read(app).closing_app_for_test()));
    assert!(cx.debug_bounds("app.close.question").is_some());
    assert!(cx.debug_bounds("app.close.task.0").is_some());
    assert!(cx.debug_bounds("app.close.task.1").is_some());
    let first_line = cx
        .update(|_window, app| view.read(app).interrupt_line_for_test(first_task))
        .expect("the first task is listed");
    let second_line = cx
        .update(|_window, app| view.read(app).interrupt_line_for_test(second_task))
        .expect("the second task is listed");
    assert!(first_line.contains("C:/work/a.txt"), "{first_line}");
    assert!(second_line.contains("C:/work/b.txt"), "{second_line}");
    assert!(
        first_transfers.cancels().is_empty() && second_transfers.cancels().is_empty(),
        "asking does not stop either transfer"
    );

    click(cx, "app.close.cancel");
    assert!(!cx.update(|_window, app| view.read(app).closing_app_for_test()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(first)),
        1
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(second)),
        1
    );

    assert!(!cx.simulate_close());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("app.close.confirm").is_some(),
        "confirming is an explicit action"
    );
    // The confirmation removes the window, so the cancels are read from the
    // channels after it returns and the window is not drawn again.
    cx.update(|window, app| {
        view.update(app, |view, cx| view.confirm_close_app_for_test(window, cx));
    });
    assert_eq!(first_transfers.cancels(), vec![first_task]);
    assert_eq!(second_transfers.cancels(), vec![second_task]);
}

// ---------------------------------------------------------------------------
// Window lifecycle on macOS (R13)
// ---------------------------------------------------------------------------

#[test]
fn closing_the_window_hides_on_macos_and_closes_everywhere_else() {
    assert_eq!(
        close_intent(Platform::MacOs, false),
        CloseIntent::Hide,
        "the red button on macOS keeps the application running"
    );
    assert_eq!(
        close_intent(Platform::MacOs, true),
        CloseIntent::Close,
        "an exit the user confirmed really closes the window"
    );
    for platform in [Platform::Windows, Platform::Linux] {
        assert_eq!(
            close_intent(platform, false),
            CloseIntent::Close,
            "the close semantics of {platform:?} do not change"
        );
    }
}

#[test]
fn a_reopen_activates_a_surviving_window_and_builds_one_only_without() {
    let main = WindowId::from(7_u64);
    let other = WindowId::from(9_u64);
    assert_eq!(
        reopen_choice(Some(main), &[other, main]),
        ReopenChoice::Activate(main),
        "a window that is still alive is activated"
    );
    assert_eq!(
        reopen_choice(None, &[other]),
        ReopenChoice::OpenMain,
        "a shell that kept no window builds the main one"
    );
    assert_eq!(
        reopen_choice(Some(main), &[other]),
        ReopenChoice::OpenMain,
        "a remembered window that is gone is stale, not a reason to do nothing"
    );
    assert_eq!(
        reopen_choice(Some(main), &[]),
        ReopenChoice::OpenMain,
        "with nothing on screen the main window is created"
    );
}

#[gpui::test]
async fn a_macos_close_hides_the_window_and_keeps_its_sessions(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let Some(session) = start_local_session(cx, &view) else {
        return;
    };
    let before = cx.update(|_window, app| view.read(app).open_sessions());
    assert_eq!(before, vec![session]);

    // The macOS close request, asked of the same handler the window registered.
    let closed = cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.on_window_should_close(Platform::MacOs, cx)
        })
    });
    assert!(!closed, "hiding is not closing: the window is kept");
    assert!(
        cx.update(|_window, app| view.read(app).is_hidden_for_reopen()),
        "the window is marked hidden, so a reopen knows to bring it back"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).closing_app_for_test()),
        "hiding never claims that running tasks are about to stop"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_sessions()),
        before,
        "the session survives the close"
    );
    assert_ne!(
        session_state(cx, &view, session),
        Some(SessionState::Closed),
        "the terminal of the hidden window is still alive"
    );

    // A Dock click while that window is alive activates it instead of building another.
    let windows = cx.update(|_window, app| app.windows().len());
    // The application-level handler, not the window's: the window is checked out for the
    // duration of a `VisualTestContext` update, and a reopen has to see the real one.
    cx.cx.update(|app| {
        let handle = app
            .windows()
            .first()
            .and_then(|handle| handle.downcast::<ShelluneView>())
            .expect("the test window shows the shell");
        app.set_global(crate::app::MainWindow(Some(handle)));
        crate::app::reopen_main_window(app).expect("the hidden window comes back");
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| app.windows().len()),
        windows,
        "reopening does not add a second window"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).is_hidden_for_reopen()),
        "the window is in front again"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_sessions()),
        before,
        "no session is started or repeated by the reopen"
    );
}

/// Asks the application-level handler a question the platform would ask (R9, R13).
///
/// The window is checked out for the duration of a `VisualTestContext` update, and the
/// application-level handlers have to see the real window, so they run through the app
/// and the applied handle is registered as the main one first.
fn ask_application<R>(cx: &mut TestAppContext, ask: impl FnOnce(&mut gpui::App) -> R) -> R {
    cx.update(|app| {
        let handle = app
            .windows()
            .first()
            .and_then(|handle| handle.downcast::<ShelluneView>())
            .expect("the test window shows the shell");
        app.set_global(crate::app::MainWindow(Some(handle)));
        ask(app)
    })
}

#[gpui::test]
async fn a_termination_with_running_transfers_is_refused_then_allowed(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);

    // Nothing is running, so an ordinary quit is not slowed down by a question.
    assert!(
        ask_application(cx, crate::app::allow_termination),
        "a shell with no running work lets the application quit"
    );

    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/c.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;

    // Cmd+Q arrives as a termination request, not as a window close.
    assert!(
        !ask_application(cx, crate::app::allow_termination),
        "a termination with a running transfer is refused"
    );
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).closing_app_for_test()),
        "the refusal asks the question instead of quitting"
    );
    assert!(
        cx.update(|_window, app| view.read(app).terminate_pending_for_test()),
        "the refusal is remembered, because the question is drawn in the window"
    );
    assert!(
        cx.update(|_window, app| view.read(app).open_sessions())
            .contains(&id),
        "the session survives the refused termination"
    );
    assert!(
        transfers.cancels().is_empty(),
        "asking does not stop the transfer"
    );

    click(cx, "app.close.cancel");
    assert!(!cx.update(|_window, app| view.read(app).closing_app_for_test()));
    assert!(
        !cx.update(|_window, app| view.read(app).terminate_pending_for_test()),
        "cancelling the question also forgets the refused termination"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        1,
        "the cancelled question leaves the transfer running"
    );

    // A window the red button hid still has to show the question, or the quit looks
    // broken: the refusal reveals it instead of drawing into a hidden window.
    let hidden = cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.on_window_should_close(Platform::MacOs, cx)
        })
    });
    assert!(!hidden, "the red button hides the window");
    assert!(cx.update(|_window, app| view.read(app).is_hidden_for_reopen()));
    assert!(
        !ask_application(cx, crate::app::allow_termination),
        "a Cmd+Q of a hidden window with a running transfer is refused"
    );
    cx.run_until_parked();
    assert!(
        !cx.update(|_window, app| view.read(app).is_hidden_for_reopen()),
        "the window with the question is put back on screen"
    );
    assert!(
        cx.debug_bounds("app.close.question").is_some(),
        "the question is drawn where the user can answer it"
    );
    click(cx, "app.close.cancel");
    assert_eq!(
        cx.update(|_window, app| view.read(app).transfer_active_count(id)),
        1,
        "the second cancelled question leaves the transfer running"
    );

    // Asking again is refused again: the transfer is still there.
    assert!(
        !ask_application(cx, crate::app::allow_termination),
        "a third Cmd+Q asks the same question"
    );
    cx.run_until_parked();

    // Confirming stops the work and asks once more, which is now allowed.
    cx.update(|window, app| {
        view.update(app, |view, cx| view.confirm_close_app_for_test(window, cx));
    });
    assert_eq!(transfers.cancels(), vec![task]);
    assert!(
        cx.update(|_window, app| view.read(app).app_close_confirmed_for_test()),
        "the answer is remembered for the termination that follows"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).terminate_pending_for_test()),
        "the request is not left waiting once it was answered"
    );
    assert!(
        ask_application(cx, crate::app::allow_termination),
        "the termination asked after the confirmation really ends the application"
    );
}

#[gpui::test]
async fn a_dock_reopen_restores_a_minimized_window(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let windows = cx.update(|_window, app| app.windows().len());

    cx.update(|window, _app| window.minimize_window());
    assert!(
        cx.update(|window, _app| window.is_minimized_window()),
        "the window is in the Dock"
    );

    ask_application(cx, |app| {
        crate::app::reopen_main_window(app).expect("the minimized window comes back");
    });
    cx.run_until_parked();
    assert!(
        !cx.update(|window, _app| window.is_minimized_window()),
        "activating the application is not enough: the window itself is restored"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).is_hidden_for_reopen()),
        "the window is on screen, not waiting for the icon"
    );
    assert_eq!(
        cx.update(|_window, app| app.windows().len()),
        windows,
        "restoring a minimized window does not build a second one"
    );
}

// ---------------------------------------------------------------------------
// Command palette and tab handling
// ---------------------------------------------------------------------------

/// Opens two real shells from the header, or returns `None` when the machine has none.
fn two_shells(cx: &mut TestAppContext) -> Option<(Entity<ShelluneView>, &mut VisualTestContext)> {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return None;
    }
    click(cx, "header.primary");
    click(cx, "header.primary");
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs().len()),
        2
    );
    Some((view, cx))
}

fn palette_keystroke() -> &'static str {
    Platform::current().palette_keystroke()
}

#[gpui::test]
async fn the_palette_opens_from_a_terminal_and_switches_sessions(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[1]),
        "the newest shell is on screen and has the keyboard"
    );

    cx.simulate_keystrokes(palette_keystroke());
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).palette_open()),
        "the palette shortcut works while a terminal has the keyboard"
    );
    assert!(cx.debug_bounds("palette").is_some());

    // Typing filters; the first match is highlighted and Enter runs it.
    cx.simulate_input("1");
    cx.run_until_parked();
    let titles = cx.update(|_window, app| view.read(app).palette_titles());
    assert!(
        titles.first().is_some_and(|title| title.ends_with(" 1")),
        "the first tab is the best match for its number, got {titles:?}"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).palette_open()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[0]),
        "Enter switched to the session that was picked"
    );
}

#[gpui::test]
async fn escape_closes_the_palette_without_running_anything(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    focus_root(&view, cx);

    go(cx, crate::app::actions::OpenPalette);
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
    cx.simulate_input("hosts");
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).palette_open()));
    assert_eq!(
        destination_of(&view, cx),
        Destination::LocalShells,
        "closing the palette does not run the highlighted entry"
    );

    // The arrow keys move the highlight; Enter runs what is highlighted.
    cx.simulate_keystrokes(palette_keystroke());
    cx.simulate_input("hosts");
    cx.run_until_parked();
    let titles = cx.update(|_window, app| view.read(app).palette_titles());
    assert_eq!(
        titles.first().map(String::as_str),
        Some(ENGLISH.nav_hosts),
        "a page name is matched by its title first"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Hosts);
}

#[gpui::test]
async fn a_search_without_a_match_says_so(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    go(cx, crate::app::actions::OpenPalette);
    cx.simulate_input("zzqqxx");
    cx.run_until_parked();
    assert!(cx.update(|_window, app| view.read(app).palette_titles().is_empty()));
    assert!(cx.debug_bounds("palette.empty").is_some());
    // Enter over an empty list does nothing and keeps the palette open for a new search.
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
}

#[gpui::test]
async fn tab_shortcuts_cycle_jump_and_close(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let platform = Platform::current();
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let active =
        |cx: &mut VisualTestContext| cx.update(|_window, app| view.read(app).active_session());

    // Next wraps from the last tab to the first; a second press keeps working, which
    // it only does when the keyboard followed the tab that was shown.
    let (next, previous) = platform.tab_cycle_keystrokes();
    cx.simulate_keystrokes(next);
    cx.run_until_parked();
    assert_eq!(active(cx), Some(tabs[0]));
    cx.simulate_keystrokes(next);
    cx.run_until_parked();
    assert_eq!(active(cx), Some(tabs[1]));
    cx.simulate_keystrokes(previous);
    cx.run_until_parked();
    assert_eq!(active(cx), Some(tabs[0]));

    cx.simulate_keystrokes(&format!("{}-2", platform.tab_jump_modifier()));
    cx.run_until_parked();
    assert_eq!(active(cx), Some(tabs[1]), "the digit jumps to that tab");
    cx.simulate_keystrokes(&format!("{}-9", platform.tab_jump_modifier()));
    cx.run_until_parked();
    assert_eq!(
        active(cx),
        Some(tabs[1]),
        "the last digit means the last tab"
    );

    cx.simulate_keystrokes(platform.close_tab_keystroke());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        vec![tabs[0]],
        "only the tab on screen was closed"
    );
    assert_eq!(active(cx), Some(tabs[0]), "the remaining tab took over");
}

#[gpui::test]
async fn the_tab_menu_closes_the_other_tabs_and_duplicates_one(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());

    // Right-click the first tab.
    let bounds = bounds_of(cx, "session.tab.0");
    let center = bounds.center();
    cx.simulate_mouse_down(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("tab.menu").is_some(),
        "a right click opens the tab menu"
    );

    // Escape closes it, even though a terminal had the keyboard.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    let menu_open = cx.update(|_window, app| view.read(app).tab_menu_open());
    assert!(!menu_open, "Escape closes the tab menu");

    cx.simulate_mouse_down(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.run_until_parked();
    click(cx, "tab.menu.close_others");
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        vec![tabs[0]],
        "every tab but the one the menu belongs to was closed"
    );

    cx.simulate_mouse_down(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(center.x, center.y),
        MouseButton::Right,
        Modifiers::default(),
    );
    cx.run_until_parked();
    click(cx, "tab.menu.duplicate");
    let after = cx.update(|_window, app| view.read(app).open_tabs());
    assert_eq!(after.len(), 2, "duplicating opened another tab");
    let kinds = cx.update(|_window, app| {
        after
            .iter()
            .map(|id| {
                view.read(app)
                    .workspace()
                    .session(*id)
                    .map(|s| s.kind().clone())
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds[0], kinds[1], "the copy runs the same shell");
}

#[gpui::test]
async fn closing_a_mixed_tab_from_the_menu_drops_its_pending_pane(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let (kept, closing) = (tabs[0], tabs[1]);
    let pending = cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.split_pending_for_test(
                SplitAxis::LeftRight,
                PaneTarget::local(Some("shellune-pending-leaf".to_owned())),
                window,
                cx,
            )
        })
    });
    assert!(
        pending.is_some(),
        "the split adds a pane that has no session"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        1
    );

    right_click(cx, "session.tab.1");
    click(cx, "tab.menu.close");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(
            view.pending_pane_count_for_test(),
            0,
            "the waiting pane is removed with its tab"
        );
        assert_eq!(
            view.layout_tab_count_for_test(),
            1,
            "the tab itself is gone"
        );
        assert_eq!(view.open_tabs(), vec![kept]);
        assert!(
            view.workspace()
                .session(kept)
                .is_some_and(|session| matches!(session.state(), SessionState::Connected)),
            "the other tab's session is still running"
        );
        assert!(
            view.workspace().session(closing).is_none()
                || view
                    .workspace()
                    .session(closing)
                    .is_some_and(|session| !matches!(session.state(), SessionState::Connected)),
            "the closed session is no longer a live pane"
        );
    });

    // Close others and close right share the same removal. A waiting pane on a
    // tab that is not the one under the pointer must go with that tab.
    click(cx, "header.primary");
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.split_pending_for_test(
                SplitAxis::TopBottom,
                PaneTarget::remote(shellune_core::HostId::new()),
                window,
                cx,
            );
        });
    });
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        1
    );
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.close_others");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(view.pending_pane_count_for_test(), 0);
        assert_eq!(view.layout_tab_count_for_test(), 1);
        assert_eq!(view.open_tabs(), vec![kept]);
    });

    click(cx, "header.primary");
    click(cx, "header.primary");
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.split_pending_for_test(
                SplitAxis::LeftRight,
                PaneTarget::local(Some("shellune-pending-right".to_owned())),
                window,
                cx,
            );
        });
    });
    assert_eq!(
        cx.update(|_window, app| view.read(app).layout_tab_count_for_test()),
        3
    );
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.close_right");
    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(
            view.pending_pane_count_for_test(),
            0,
            "a waiting pane on a tab to the right is removed"
        );
        assert_eq!(view.layout_tab_count_for_test(), 1);
        assert_eq!(view.open_tabs(), vec![kept]);
    });
}

#[gpui::test]
async fn the_tab_menu_names_a_tab_and_restores_its_automatic_title(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let first = tabs[0];
    let tab = cx
        .update(|_window, app| view.read(app).workspace().tab_of_session(first))
        .expect("the session has a tab");
    let automatic = cx.update(|_window, app| view.read(app).tab_label_for_test(first));

    // A name is given from the tab's own menu, and only the label changes.
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    assert!(
        cx.update(|_window, app| view.read(app).tab_rename_open()),
        "the name dialog opens"
    );
    cx.simulate_input("deploy");
    cx.run_until_parked();
    click(cx, "tab.rename.save");
    assert!(
        !cx.update(|_window, app| view.read(app).tab_rename_open()),
        "saving closes the dialog"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .tab_custom_title(tab)
            .map(str::to_owned)),
        Some("deploy".to_owned())
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(first)),
        "deploy"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .layout()
            .tab(tab)
            .map(|tab| tab.title().to_owned())),
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .session(first)
            .map(|session| session.title().to_owned())),
        "the automatic title is kept behind the name"
    );
    assert!(
        cx.update(|_window, app| view.read(app).workspace().session(first).is_some()),
        "naming a tab does not change its session"
    );
    // The palette reads the same name, so the two places cannot disagree.
    let titles = cx.update(|_window, app| view.read(app).tab_label_for_test(tabs[1]));
    assert_ne!(titles, "deploy", "the other tab is untouched");

    // Restoring the automatic title clears only the name.
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    click(cx, "tab.rename.auto");
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .tab_custom_title(tab)
            .map(str::to_owned)),
        None
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(first)),
        automatic
    );

    // An empty name is refused with a reason, and the field keeps what was typed so
    // the user can add the missing letter.
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    click(cx, "tab.rename.save");
    assert!(
        cx.update(|_window, app| view.read(app).tab_rename_open()),
        "an empty name leaves the dialog open"
    );
    assert!(
        cx.update(|_window, app| view.read(app).tab_rename_problem().map(str::to_owned))
            .is_some_and(|problem| !problem.trim().is_empty()),
        "and says why"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).tab_rename_open()));

    // A dialog that outlives its tab closes with it instead of naming something gone.
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    assert!(cx.update(|_window, app| view.read(app).tab_rename_open()));
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.close_session(first, cx));
    });
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).tab_rename_open()));
}

#[gpui::test]
async fn a_tab_colour_is_chosen_and_cleared_without_touching_its_state(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let (first, second) = (tabs[0], tabs[1]);
    let second_tab = cx
        .update(|_window, app| view.read(app).workspace().tab_of_session(second))
        .expect("the session has a tab");
    let first_tab = cx
        .update(|_window, app| view.read(app).workspace().tab_of_session(first))
        .expect("the session has a tab");

    right_click(cx, "session.tab.1");
    assert!(
        cx.debug_bounds("tab.menu.accents").is_some(),
        "the tab menu offers the colours"
    );
    assert!(
        cx.debug_bounds("session.tab.accent.1").is_none(),
        "no tab carries a mark before one is chosen"
    );
    click(cx, "tab.menu.accent.teal");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().tab_accent(second_tab)),
        Some(shellune_core::TabAccent::Teal)
    );
    assert!(
        cx.debug_bounds("session.tab.accent.1").is_some(),
        "the coloured tab carries its mark"
    );
    assert!(
        cx.debug_bounds("session.tab.accent.0").is_none(),
        "the other tab has no mark"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().tab_accent(first_tab)),
        None
    );
    // The state is still readable: the live session keeps its state, and its tab
    // still shows the close button the colour must not have replaced.
    assert!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .session(second)
            .is_some_and(|session| matches!(session.state(), SessionState::Connected))),
        "a colour does not change what the session is doing"
    );
    assert!(cx.debug_bounds("session.tab.close.1").is_some());

    // Clearing the colour leaves the tab exactly as it was otherwise.
    click(cx, "tab.menu.accent.none");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().tab_accent(second_tab)),
        None
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().tab_accent(first_tab)),
        None,
        "the other tab never got a colour"
    );
    assert!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .session(second)
            .is_some_and(|session| matches!(session.state(), SessionState::Connected))),
        "clearing a colour does not change the session either"
    );
    let labels = cx.update(|_window, app| {
        (
            view.read(app).tab_label_for_test(first),
            view.read(app).tab_label_for_test(second),
        )
    });
    assert!(!labels.0.is_empty() && !labels.1.is_empty());
}

#[gpui::test]
async fn a_colour_swatch_answers_the_pointer_in_both_rows(cx: &mut TestAppContext) {
    // A swatch is a control, so it has to look like one under the pointer, in the tab
    // menu and in the name dialog alike (R2, R3). The drawing reads the same value the
    // pointer sets, so this checks the appearance, not just that the swatch is clickable.
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let second = tabs[1];
    let tab = cx
        .update(|_window, app| view.read(app).workspace().tab_of_session(second))
        .expect("the session has a tab");
    let face = |cx: &mut VisualTestContext, row: AccentRow, accent| {
        cx.update(|_window, app| view.read(app).accent_face_for_test(row, tab, accent))
    };
    let hovered = |cx: &mut VisualTestContext| {
        cx.update(|_window, app| view.read(app).accent_hover_for_test())
    };

    // The tab menu's row.
    right_click(cx, "session.tab.1");
    assert_eq!(
        hovered(cx),
        None,
        "nothing is hovered until the pointer moves"
    );
    let resting = face(cx, AccentRow::Menu, shellune_core::TabAccent::Teal);
    assert_eq!(
        resting,
        theme::accent_swatch_face(false, theme::SwatchState::Resting)
    );
    assert_eq!(
        resting.fill, None,
        "a swatch at rest has no fill of its own"
    );

    hover(cx, "tab.menu.accent.teal");
    assert_eq!(
        hovered(cx),
        Some((AccentRow::Menu, shellune_core::TabAccent::Teal)),
        "the swatch under the pointer takes the hover"
    );
    let pointed = face(cx, AccentRow::Menu, shellune_core::TabAccent::Teal);
    assert_ne!(pointed, resting, "hovering changes how the swatch is drawn");
    assert_eq!(
        pointed.fill,
        Some(palette::SURFACE_HOVER),
        "the fill under the pointer is the surface the other controls use"
    );
    assert_eq!(
        pointed.ring,
        Some(palette::BORDER_STRONG),
        "and its ring is the hover border"
    );
    assert_eq!(
        face(cx, AccentRow::Menu, shellune_core::TabAccent::Lime),
        resting,
        "only the swatch under the pointer is drawn as hovered"
    );

    hover(cx, "tab.menu.rename");
    assert_eq!(hovered(cx), None, "leaving the swatch ends its hover");

    // The chosen colour keeps its ring while the pointer is on another swatch. Choosing
    // a colour leaves the menu open, so the row can be used again straight away.
    click(cx, "tab.menu.accent.teal");
    assert!(
        cx.update(|_window, app| view.read(app).tab_menu_open()),
        "choosing a colour does not close the menu"
    );
    assert_eq!(
        face(cx, AccentRow::Menu, shellune_core::TabAccent::Teal).ring,
        Some(palette::TEXT_PRIMARY),
        "the chosen swatch keeps its ring at rest"
    );
    hover(cx, "tab.menu.accent.lime");
    assert_eq!(
        face(cx, AccentRow::Menu, shellune_core::TabAccent::Teal).ring,
        Some(palette::TEXT_PRIMARY),
        "and keeps it while the pointer rests on a neighbour"
    );
    assert_eq!(
        face(cx, AccentRow::Menu, shellune_core::TabAccent::Lime).fill,
        Some(palette::SURFACE_HOVER),
        "while the neighbour answers the pointer"
    );

    // The same row in the name dialog.
    click(cx, "tab.menu.rename");
    assert!(
        cx.update(|_window, app| view.read(app).tab_rename_open()),
        "the menu's own row opens the name dialog"
    );
    assert!(
        cx.debug_bounds("tab.rename.accent.teal").is_some(),
        "the dialog offers the colours"
    );
    let dialog_resting = face(cx, AccentRow::Rename, shellune_core::TabAccent::Orchid);
    hover(cx, "tab.rename.accent.orchid");
    assert_eq!(
        hovered(cx),
        Some((AccentRow::Rename, shellune_core::TabAccent::Orchid)),
        "the dialog's swatch answers the pointer too"
    );
    let dialog_pointed = face(cx, AccentRow::Rename, shellune_core::TabAccent::Orchid);
    assert_ne!(
        dialog_pointed, dialog_resting,
        "and changes how it is drawn, exactly like the menu's"
    );
    assert_eq!(dialog_pointed.fill, Some(palette::SURFACE_HOVER));
    assert_eq!(
        hovered(cx),
        Some((AccentRow::Rename, shellune_core::TabAccent::Orchid)),
        "the dialog's hover is the dialog's, not the menu's"
    );
}

#[test]
fn a_pressed_swatch_is_drawn_between_the_resting_and_the_pressed_surfaces() {
    // The pointer's press is a GPUI pseudo-state the test window cannot paint, so the
    // face it draws is checked on the tokens the view paints it with.
    for chosen in [false, true] {
        let resting = theme::accent_swatch_face(chosen, theme::SwatchState::Resting);
        let hovered = theme::accent_swatch_face(chosen, theme::SwatchState::Hovered);
        let pressed = theme::accent_swatch_face(chosen, theme::SwatchState::Pressed);
        assert_eq!(
            pressed.fill,
            Some(palette::SURFACE_SELECTED),
            "press goes one step past hover"
        );
        assert_ne!(pressed.fill, hovered.fill, "a press is not a hover");
        assert_ne!(
            pressed.fill, resting.fill,
            "and not a resting swatch either"
        );
        assert!(
            pressed.ring.is_some(),
            "a pressed swatch still has a ring, so it never looks unfinished"
        );
        if chosen {
            for face in [resting, hovered, pressed] {
                assert_eq!(
                    face.ring,
                    Some(palette::TEXT_PRIMARY),
                    "the chosen colour keeps its own ring in every state"
                );
            }
        } else {
            assert_ne!(
                pressed.ring, hovered.ring,
                "an unchosen swatch says the press in its ring as well"
            );
        }
    }
}

#[gpui::test]
async fn a_name_past_the_limit_says_what_it_keeps(cx: &mut TestAppContext) {
    // The limit stays, but it is not silent: the dialog says so while the name can
    // still be shortened, and again when it saved a shortened one (R2, R3).
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let first = tabs[0];
    let tab = cx
        .update(|_window, app| view.read(app).workspace().tab_of_session(first))
        .expect("the session has a tab");
    let automatic = cx.update(|_window, app| view.read(app).tab_label_for_test(first));

    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    cx.simulate_input("deploy");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_rename_limit_for_test(app)),
        None,
        "a name inside the limit says nothing about it"
    );

    // Ten characters short of twice the limit: too long, and said so before saving.
    let long: String = "abcdefghij".repeat(8);
    cx.simulate_input(&long);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_rename_limit_for_test(app)),
        Some(crate::app::view::TabRenameLimit::Typing),
        "the dialog says the name is too long before it is saved"
    );
    assert!(
        cx.debug_bounds("tab.rename.limit").is_some(),
        "and that line is drawn in the dialog"
    );
    assert_fits_inside(
        bounds_of(cx, "tab.rename.limit"),
        design_window(),
        "the length notice",
    );

    click(cx, "tab.rename.save");
    let kept: String = format!("deploy{long}")
        .chars()
        .take(shellune_core::MAX_TAB_NAME_CHARS)
        .collect();
    assert_eq!(
        kept.chars().count(),
        shellune_core::MAX_TAB_NAME_CHARS,
        "the name really was longer than a tab can hold"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .tab_custom_title(tab)
            .map(str::to_owned)),
        Some(kept.clone()),
        "the tab kept the first characters of what was typed"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(first)),
        kept,
        "the strip shows what was kept, never the longer name that was typed"
    );
    assert!(
        cx.debug_bounds("tab.rename.shortened").is_some(),
        "and the dialog says what it kept"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_rename_limit_for_test(app)),
        Some(crate::app::view::TabRenameLimit::Saved),
        "the field now holds a name inside the limit, so the warning about typing is \
         replaced by the line about what was kept"
    );
    assert!(
        cx.update(|_window, app| view.read(app).tab_rename_open()),
        "the dialog stays open on the name that was kept instead of closing over it"
    );

    // A second save has nothing left to shorten, so it closes the dialog.
    click(cx, "tab.rename.save");
    assert!(!cx.update(|_window, app| view.read(app).tab_rename_open()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(first)),
        kept,
        "saving again keeps the same name"
    );

    // The automatic title is still behind the name, so restoring it is one control away.
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    click(cx, "tab.rename.auto");
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(first)),
        automatic,
        "restoring the automatic title still works after a shortened name"
    );
}

#[gpui::test]
async fn the_palette_restores_the_title_and_clears_the_colour_without_a_pointer(
    cx: &mut TestAppContext,
) {
    // Stepping the colours never comes back to none, and a name is only cleared from a
    // control. Both are in the palette, so the keyboard alone can undo them (R2, R4).
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let active = tabs[1];
    let tab = cx
        .update(|_window, app| view.read(app).workspace().tab_of_session(active))
        .expect("the session has a tab");
    let automatic = cx.update(|_window, app| view.read(app).tab_label_for_test(active));

    // Set up with the pointer, because the point of the test is undoing it by keyboard.
    right_click(cx, "session.tab.1");
    click(cx, "tab.menu.accent.orchid");
    click(cx, "tab.menu.rename");
    cx.simulate_input("deploy");
    cx.run_until_parked();
    click(cx, "tab.rename.save");
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(active)),
        "deploy"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().tab_accent(tab)),
        Some(shellune_core::TabAccent::Orchid)
    );

    // The automatic title, from the palette and nothing else.
    cx.simulate_keystrokes(palette_keystroke());
    cx.simulate_input("automatic");
    cx.run_until_parked();
    let titles = cx.update(|_window, app| view.read(app).palette_titles());
    assert_eq!(
        titles.first().map(String::as_str),
        Some(ENGLISH.palette_restore_tab_title),
        "the palette offers the automatic title first, got {titles:?}"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(active)),
        automatic,
        "the tab shows its automatic title again"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().tab_accent(tab)),
        Some(shellune_core::TabAccent::Orchid),
        "restoring the title keeps the colour"
    );

    // No colour, also from the palette alone.
    cx.simulate_keystrokes(palette_keystroke());
    cx.simulate_input("clear");
    cx.run_until_parked();
    let titles = cx.update(|_window, app| view.read(app).palette_titles());
    assert_eq!(
        titles.first().map(String::as_str),
        Some(ENGLISH.palette_clear_tab_accent),
        "the palette offers clearing the colour first, got {titles:?}"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().tab_accent(tab)),
        None,
        "the tab has no distinguishing colour again"
    );
    assert!(
        cx.update(|_window, app| view
            .read(app)
            .workspace()
            .session(active)
            .is_some_and(|session| matches!(session.state(), SessionState::Connected))),
        "neither command touches what the session is doing"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(active)),
        automatic,
        "and clearing the colour leaves the name alone"
    );
}

#[gpui::test]
async fn the_tab_list_shows_every_tab_and_switches_to_it(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[1])
    );
    let viewport = minimum_window();
    cx.simulate_resize(viewport);
    cx.run_until_parked();
    assert_fits_inside(
        bounds_of(cx, "tabs.list.open"),
        viewport,
        "the all-tabs control",
    );

    click(cx, "tabs.list.open");
    assert!(
        cx.debug_bounds("tabs.list").is_some(),
        "the list opens over the page"
    );
    assert!(cx.debug_bounds("tabs.list.row.0").is_some());
    assert!(cx.debug_bounds("tabs.list.row.1").is_some());
    assert!(
        cx.update(|_window, app| view.read(app).tab_list_open()),
        "the list is open"
    );

    // Choosing a row shows that tab, and the list closes behind it.
    click(cx, "tabs.list.row.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[0])
    );
    assert!(!cx.update(|_window, app| view.read(app).tab_list_open()));
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);

    // Escape closes the list without switching anything.
    click(cx, "tabs.list.open");
    assert!(cx.debug_bounds("tabs.list").is_some());
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).tab_list_open()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[0]),
        "closing the list did not change the tab on screen"
    );
}

#[gpui::test]
async fn the_tab_list_is_walked_with_the_keyboard_and_shows_the_shortcut_in_force(
    cx: &mut TestAppContext,
) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    click(cx, "tabs.list.open");
    assert!(
        cx.update(|_window, app| view.read(app).tab_list_open()),
        "the list is open"
    );
    // It opens on the tab that is already on screen, so the arrows start from there.
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_list_selected()),
        1,
        "the row the keyboard starts on is the tab on screen"
    );
    assert!(
        cx.debug_bounds("tabs.list.jump.0").is_some(),
        "a row shows the shortcut that reaches that tab, read from the bindings in force"
    );

    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_list_selected()),
        0,
        "the arrow keys move the highlight"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        !cx.update(|_window, app| view.read(app).tab_list_open()),
        "Enter opens the highlighted row and closes the list"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[0]),
        "the highlighted tab is the one on screen"
    );

    // Escape closes it without switching anything.
    click(cx, "tabs.list.open");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).tab_list_open()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[0]),
        "closing the list did not change the tab on screen"
    );
}

#[gpui::test]
async fn the_tab_name_dialog_and_the_tab_list_fit_the_smallest_window_at_large_text(
    cx: &mut TestAppContext,
) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let viewport = minimum_window();
    cx.simulate_resize(viewport);
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest,
        "the dialogs are measured at 200% text"
    );

    // At this size the strip itself has no room left, so the list of every tab is the
    // way to a tab; its pencil opens the same name dialog.
    assert!(
        cx.debug_bounds("session.tab.0").is_some(),
        "the tab is still laid out, even when the strip has to clip it"
    );
    assert_fits_inside(
        bounds_of(cx, "tabs.list.open"),
        viewport,
        "the all-tabs control",
    );
    assert_pointer_target(cx, "tabs.list.open");
    click(cx, "tabs.list.open");
    assert_fits_inside(bounds_of(cx, "tabs.list.dialog"), viewport, "the tab list");
    assert_fits_inside(
        bounds_of(cx, "modal.subtitle"),
        viewport,
        "the tab list subtitle",
    );
    // The first row is on screen; the rest are reached by the dialog's own scroll, so
    // the test asks that each one is rendered rather than that all fit at once.
    assert_fits_inside(
        bounds_of(cx, "tabs.list.row.0"),
        viewport,
        "the first tab row",
    );
    for index in 0..2 {
        let selector: &'static str = Box::leak(format!("tabs.list.row.{index}").into_boxed_str());
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} is in the list"
        );
        let rename: &'static str = Box::leak(format!("tabs.list.rename.{index}").into_boxed_str());
        assert_pointer_target(cx, rename);
    }
    let close: &'static str = "tabs.list.close";
    assert_fits_inside(bounds_of(cx, close), viewport, close);

    click(cx, "tabs.list.rename.0");
    assert!(
        cx.update(|_window, app| view.read(app).tab_rename_open()),
        "the pencil in the tab list names the tab it belongs to"
    );
    // The name dialog keeps its field and its buttons on screen; the colours sit in
    // its scrolling body, so they are reached by scrolling rather than being cut off.
    // Cancel and "restore the automatic title" are the other footer actions, so they
    // are measured the same way as save rather than assumed to follow it (R4).
    assert_fits_inside(
        bounds_of(cx, "tab.rename.dialog"),
        viewport,
        "the name dialog",
    );
    for selector in [
        "tab.rename.field",
        "tab.rename.save",
        "tab.rename.cancel",
        "tab.rename.auto",
    ] {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    for selector in ["tab.rename.accent.teal", "tab.rename.accent.none"] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} is drawn in the dialog"
        );
    }
    assert_pointer_target(cx, "tab.rename.accent.teal");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).tab_rename_open()));
}

#[gpui::test]
async fn the_tab_identity_rows_stay_inside_their_cards_and_are_reached_by_scrolling(
    cx: &mut TestAppContext,
) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    // A tab that carries a name and a colour, so the rows holding them are the measured
    // ones. Both are set through the UI, the way a person sets them.
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let first = tabs[0];
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.accent.teal");
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    cx.simulate_input("deploy");
    cx.run_until_parked();
    click(cx, "tab.rename.save");
    assert_eq!(
        cx.update(|_window, app| view.read(app).tab_label_for_test(first)),
        "deploy",
        "the tab is named before the boundary size is measured"
    );

    // The smallest window at the largest text: there is no room to spare anywhere, so a
    // row that does not wrap is a row that is drawn past the card that holds it.
    let viewport = minimum_window();
    cx.simulate_resize(viewport);
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }

    // The list of every tab: each row stays inside the dialog's own width.
    click(cx, "tabs.list.open");
    cx.run_until_parked();
    let list = bounds_of(cx, "tabs.list.dialog");
    let list_body = bounds_of(cx, "tabs.list.dialog.body");
    assert_fits_inside(list, viewport, "the tab list");
    assert_inside_horizontally(list_body, list, "the tab list's body");
    assert!(
        list_body.size.height >= px(24.0),
        "the body the rows live in has no line to stand on: {list_body:?}"
    );
    let first_row = bounds_of(cx, "tabs.list.row.0");
    assert_inside_horizontally(first_row, list_body, "the first tab row");
    for selector in [
        "tabs.list.accent.0",
        "tabs.list.accent-name.0",
        "tabs.list.rename.0",
    ] {
        assert_inside_horizontally(bounds_of(cx, selector), first_row, selector);
    }
    assert_pointer_target(cx, "tabs.list.rename.0");
    let next_row = bounds_of(cx, "tabs.list.row.1");
    assert_inside_horizontally(next_row, list_body, "the second tab row");
    assert_inside_horizontally(
        bounds_of(cx, "tabs.list.rename.1"),
        next_row,
        "the second row's pencil",
    );
    // Wrapped rather than cut: the row is taller than the controls it holds, which is
    // what one line of them would be, so its contents took a second line inside it.
    let pencil = bounds_of(cx, "tabs.list.rename.0");
    assert!(
        first_row.size.height > pencil.size.height + px(8.0),
        "the row should wrap at 200% text rather than stay one clipped line: \
         {first_row:?} for a {pencil:?} pencil"
    );

    // The name dialog, opened from the list's pencil: the field the dialog exists for is
    // what the body has room for on screen ...
    click(cx, "tabs.list.rename.0");
    let rename = bounds_of(cx, "tab.rename.dialog");
    let rename_body = bounds_of(cx, "tab.rename.dialog.body");
    assert_fits_inside(rename, viewport, "the name dialog");
    assert_inside_horizontally(rename_body, rename, "the name dialog's body");
    let field = bounds_of(cx, "tab.rename.field");
    assert!(
        field.origin.y >= rename_body.origin.y - px(0.5)
            && field.origin.y + field.size.height
                <= rename_body.origin.y + rename_body.size.height + px(0.5),
        "the name field is outside the part of the body that is on screen: {field:?} in {rename_body:?}"
    );

    // The colours stay inside the dialog's own width. The row wraps when one line
    // would run past that width, and stays one line when the dialog is wide enough.
    // Either way each swatch sits fully inside the row. The row starts at the bottom
    // of this body, so the scroll below is how the rest of a swatch is reached.
    let accents = bounds_of(cx, "tab.rename.accents");
    assert_inside_horizontally(accents, rename, "the colour row");
    for accent in ["none", "iris", "violet", "teal", "lime", "orchid", "clay"] {
        let selector: &'static str =
            Box::leak(format!("tab.rename.accent.{accent}").into_boxed_str());
        let bounds = bounds_of(cx, selector);
        assert_inside_horizontally(bounds, accents, selector);
        assert!(
            bounds.origin.y >= accents.origin.y - px(0.5)
                && bounds.origin.y + bounds.size.height
                    <= accents.origin.y + accents.size.height + px(0.5),
            "{selector} is clipped by its own row: {bounds:?} in {accents:?}"
        );
        assert_pointer_target(cx, selector);
    }
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: rename_body.center(),
        delta: gpui::ScrollDelta::Lines(point(0.0, -2.0)),
        ..Default::default()
    });
    cx.run_until_parked();
    let reached = bounds_of(cx, "tab.rename.accents");
    assert_ne!(
        reached.origin.y, accents.origin.y,
        "the dialog's body does not scroll its own colours"
    );
    assert!(
        reached.origin.y >= rename_body.origin.y - px(0.5)
            && reached.origin.y < rename_body.origin.y + rename_body.size.height,
        "the colour row did not come into the body that is on screen: {reached:?} in {rename_body:?}"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();

    // The same row in the tab menu, still at 200% text, in a window where the strip has
    // room to be pointed at.
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    right_click(cx, "session.tab.0");
    let menu = bounds_of(cx, "tab.menu");
    assert_fits_inside(menu, design_window(), "the tab menu");
    let menu_accents = bounds_of(cx, "tab.menu.accents");
    assert_inside_horizontally(menu_accents, menu, "the menu's colour row");
    for accent in ["none", "iris", "violet", "teal", "lime", "orchid", "clay"] {
        let selector: &'static str =
            Box::leak(format!("tab.menu.accent.{accent}").into_boxed_str());
        assert_inside_horizontally(bounds_of(cx, selector), menu, selector);
    }
}

#[gpui::test]
async fn a_restored_strip_of_named_tabs_stays_pending_and_whole(cx: &mut TestAppContext) {
    let dir = TempDir::new("tab-identity-restore");
    seed(&dir.store(), Vec::new());
    let names = ["build", "staging", "production", "database"];
    let accents = [
        shellune_core::TabAccent::Iris,
        shellune_core::TabAccent::Teal,
        shellune_core::TabAccent::Clay,
        shellune_core::TabAccent::Orchid,
    ];
    let mut tabs = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let program = format!("shellune-pending-{index}");
        let mut tab = WorkspaceTab::new(
            format!("automatic {index}"),
            PaneNode::leaf(
                PaneTarget::local(Some(program.clone())),
                TerminalMode::Classic,
            ),
        )
        .expect("title");
        tab.set_custom_title(Some((*name).to_owned()));
        tab.set_accent(Some(accents[index]));
        tabs.push(tab);
    }
    let snapshot = WorkspaceSnapshot::new(tabs, PanelConfig::default()).expect("snapshot");
    let ids: Vec<shellune_core::TabId> = snapshot.tabs().iter().map(WorkspaceTab::id).collect();
    WorkspaceStore::at(dir.path())
        .save(&snapshot)
        .expect("snapshot saves");

    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();

    // Nothing was started: every restored tab is waiting.
    assert!(
        cx.update(|_window, app| view.read(app).open_sessions().is_empty()),
        "restoring a strip starts no process"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        names.len()
    );
    let restored: Vec<(String, Option<shellune_core::TabAccent>)> = cx.update(|_window, app| {
        let view = view.read(app);
        view.workspace()
            .layout()
            .tabs()
            .iter()
            .map(|tab| (tab.display_title().to_owned(), tab.accent()))
            .collect()
    });
    let names_back: Vec<String> = restored.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names_back,
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
        "the names came back in the saved order"
    );
    for (index, accent) in accents.iter().enumerate() {
        assert_eq!(
            restored[index].1,
            Some(*accent),
            "tab {index} kept its colour"
        );
    }
    assert!(
        cx.debug_bounds("session.tab.accent.0").is_some(),
        "a restored tab is drawn with its colour"
    );

    // The strip cannot show every tab at the smallest window, so the list is how all
    // of them stay reachable.
    let viewport = minimum_window();
    assert_fits_inside(
        bounds_of(cx, "tabs.list.open"),
        viewport,
        "the all-tabs control",
    );
    assert_pointer_target(cx, "tabs.list.open");
    // The entries the strip could push aside keep their own room beside it.
    for selector in ["header.primary", "palette.open", "nav.settings"] {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    click(cx, "tabs.list.open");
    for index in 0..names.len() {
        let selector: &'static str = Box::leak(format!("tabs.list.row.{index}").into_boxed_str());
        assert!(cx.debug_bounds(selector).is_some(), "tab {index} is listed");
    }
    let last: &'static str =
        Box::leak(format!("tabs.list.row.{}", names.len() - 1).into_boxed_str());
    click(cx, last);
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().layout().active_tab_id()),
        Some(ids[names.len() - 1]),
        "choosing the last row shows the last tab"
    );
}

#[gpui::test]
async fn a_failed_local_adopt_keeps_the_pending_pane(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }
    click(cx, "header.primary");
    let neighbor = cx.update(|_window, app| {
        view.read(app)
            .active_session()
            .expect("the real shell is the neighbour")
    });
    let program = "shellune-no-such-shell-zz9";
    let pending = cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.split_pending_for_test(
                SplitAxis::LeftRight,
                PaneTarget::local(Some(program.to_owned())),
                window,
                cx,
            )
            .expect("the waiting pane is inserted beside the shell")
        })
    });
    let before = cx.update(|_window, app| view.read(app).leaf_ids_for_test());
    assert_eq!(
        before.len(),
        2,
        "the split has the live pane and the waiting one"
    );

    cx.update(|_window, app| {
        view.update(app, |view, cx| view.connect_leaf_for_test(pending, cx));
    });
    cx.run_until_parked();

    cx.update(|_window, app| {
        let view = view.read(app);
        assert_eq!(
            view.leaf_ids_for_test(),
            before,
            "the failed start leaves both panes where they were"
        );
        let (pending_leaf, target, session) = view
            .leaf_snapshot_for_test(pending)
            .expect("the waiting pane is still in the split");
        assert!(pending_leaf, "the pane is waiting again");
        assert_eq!(session, None, "no session is left on the pane");
        assert_eq!(target, PaneTarget::local(Some(program.to_owned())));
        assert!(
            view.workspace()
                .session(neighbor)
                .is_some_and(|session| matches!(session.state(), SessionState::Connected)),
            "the pane beside it was not closed"
        );
        assert!(
            view.workspace().sessions().items().iter().all(|session| {
                !matches!(
                    session.kind(),
                    SessionKind::Local { shell: Some(shell) } if shell.as_str() == program
                )
            }),
            "the process that never started is not a session"
        );
        let reason = view
            .local_error_for_test()
            .expect("the failure stays visible");
        assert!(
            reason.contains(program),
            "the reason names the program that did not start: {reason}"
        );
    });
    assert!(
        cx.debug_bounds("pane.pending").is_some(),
        "the pane can be tried again"
    );
    assert!(
        cx.debug_bounds("terminal.viewport").is_some(),
        "the neighbour still has its terminal"
    );
}

#[gpui::test]
async fn a_middle_click_closes_a_tab(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let bounds = bounds_of(cx, "session.tab.0");
    let center = bounds.center();
    cx.simulate_mouse_down(
        point(center.x, center.y),
        MouseButton::Middle,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        point(center.x, center.y),
        MouseButton::Middle,
        Modifiers::default(),
    );
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        vec![tabs[1]]
    );
}

#[gpui::test]
async fn the_sidebar_counts_open_sessions_not_closed_ones(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    click(cx, "session.tab.close.0");
    let open = cx.update(|_window, app| view.read(app).sidebar_count(Destination::Sessions));
    assert_eq!(open, 1, "the badge matches the tabs that are left");
}

/// Every next step the SSH layer can suggest has a Chinese sentence.
///
/// The hints live in the SSH crate, which does not know the interface language; the
/// table in `copy` translates them by their English text. Reading the hints from the
/// source keeps the two from drifting apart when a hint is added or reworded.
#[test]
fn every_ssh_next_step_is_translated() {
    // Git may check the file out with CRLF line endings (Windows), so they are
    // normalised before the function's end is looked for.
    let source = include_str!("../../crates/ssh/src/error.rs").replace("\r\n", "\n");
    let start = source
        .find("pub fn next_step(&self)")
        .expect("the SSH error type has next steps");
    let body = &source[start..];
    let body = &body[..body.find("\n    }\n").expect("the function ends")];
    let mut hints = 0;
    for part in body.split("=> ").skip(1) {
        let Some(rest) = part
            .trim_start()
            .trim_start_matches('{')
            .trim_start()
            .strip_prefix('"')
        else {
            continue;
        };
        let hint = &rest[..rest.find('"').expect("a closed string")];
        hints += 1;
        let chinese = crate::app::copy::next_step_text(hint, Locale::Chinese);
        assert_ne!(chinese, hint, "no Chinese for the SSH hint: {hint}");
    }
    assert!(
        hints >= 18,
        "only {hints} hints were found in the SSH error type"
    );
}

// ---------------------------------------------------------------------------
// Managing session records
// ---------------------------------------------------------------------------

#[gpui::test]
async fn removing_an_open_session_closes_it_first(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    cx.update(|_window, app| view.update(app, |view, cx| view.remove_session(tabs[0], cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        vec![tabs[1]],
        "the removed session's tab closed; the other one is untouched"
    );
    assert!(cx.update(|_window, app| view.read(app).workspace().session(tabs[0]).is_none()));
}

#[test]
fn only_a_closed_record_can_be_removed_from_the_model() {
    let mut workspace = Workspace::new();
    let live = connected_local_session("running");
    let live_id = live.id();
    let mut closed = connected_local_session("ended");
    closed.close().expect("a live session closes");
    let closed_id = closed.id();
    workspace.set_sessions_for_test(vec![live, closed]);

    assert_eq!(workspace.closed_sessions(), vec![closed_id]);
    assert!(
        !workspace.session_removed(live_id),
        "a session with a tab stays"
    );
    assert!(workspace.session(live_id).is_some());
    assert!(workspace.session_removed(closed_id));
    assert!(workspace.session(closed_id).is_none());
}

// ---------------------------------------------------------------------------
// The host dialog: sign-in, remembered secrets and the startup command
// ---------------------------------------------------------------------------

fn dialog_view(
    cx: &mut TestAppContext,
    hosts: Vec<HostConfig>,
) -> (Entity<ShelluneView>, &mut VisualTestContext, TempDir) {
    let dir = TempDir::new("host-dialog");
    seed(&dir.store(), hosts);
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    click(cx, "nav.hosts");
    (view, cx, dir)
}

/// Fills the dialog for a host on `127.0.0.1:1`, which refuses at once if dialled.
fn fill_new_host(cx: &mut VisualTestContext) {
    click(cx, "hosts.new");
    assert!(
        cx.debug_bounds("hosts.editor").is_some(),
        "the form opens as a dialog"
    );
    type_into(cx, "hosts.field.name", "box");
    type_into(cx, "hosts.field.address", "127.0.0.1");
    clear_field(cx, "hosts.field.port", 2);
    type_into(cx, "hosts.field.port", "1");
    type_into(cx, "hosts.field.user", "ada");
}

#[gpui::test]
async fn a_remembered_password_goes_to_the_credential_store_not_the_file(cx: &mut TestAppContext) {
    let (view, cx, dir) = dialog_view(cx, Vec::new());
    fill_new_host(cx);
    // Password is the default sign-in for a new host, and remembering is ticked.
    type_into(cx, "hosts.field.secret", "s3cret-pass");
    click(cx, "hosts.save");

    let host = cx
        .update(|_window, app| view.read(app).book().hosts().first().cloned())
        .expect("the host was saved");
    assert_eq!(host.auth(), Some(shellune_core::AuthMethodKind::Password));
    assert!(host.remembers_secret());
    assert_eq!(
        cx.update(|_window, app| view.read(app).saved_secret_for_test(host.id())),
        Some("s3cret-pass".to_owned())
    );
    let file = std::fs::read_to_string(dir.path().join(shellune_storage::FILE_NAME))
        .expect("the host file was written");
    assert!(
        !file.contains("s3cret-pass"),
        "the password never reaches the host file"
    );
    assert!(
        !cx.update(|_window, app| view.read(app).hosts_state().is_editing()),
        "a saved host closes the dialog"
    );
}

#[gpui::test]
async fn a_host_that_remembers_its_password_connects_in_one_click(cx: &mut TestAppContext) {
    let (view, cx, _dir) = dialog_view(cx, Vec::new());
    fill_new_host(cx);
    type_into(cx, "hosts.field.secret", "s3cret-pass");
    click(cx, "hosts.save");

    click(cx, "hosts.connect.0");
    let (password, busy) = cx.update(|_window, app| {
        let panel = view
            .read(app)
            .remote_state()
            .expect("the connection form exists");
        (panel.password().to_owned(), panel.status().is_busy())
    });
    assert!(
        busy,
        "nothing was left to ask, so the attempt started at once"
    );
    assert_eq!(
        password, "s3cret-pass",
        "the attempt signs in with the password from the credential store"
    );
}

#[gpui::test]
async fn saving_without_a_password_asks_for_it_when_connecting(cx: &mut TestAppContext) {
    let (view, cx, _dir) = dialog_view(cx, Vec::new());
    fill_new_host(cx);
    click(cx, "hosts.save");
    let host = cx
        .update(|_window, app| view.read(app).book().hosts().first().cloned())
        .expect("a host without a password still saves");
    assert!(
        !host.remembers_secret(),
        "nothing typed, nothing remembered"
    );

    click(cx, "hosts.connect.0");
    let busy = cx.update(|_window, app| {
        view.read(app)
            .remote_state()
            .is_some_and(|panel| panel.status().is_busy())
    });
    assert!(!busy, "the form waits for the password instead of dialling");
}

#[gpui::test]
async fn unticking_remember_or_deleting_the_host_forgets_the_secret(cx: &mut TestAppContext) {
    let (view, cx, _dir) = dialog_view(cx, Vec::new());
    fill_new_host(cx);
    type_into(cx, "hosts.field.secret", "first");
    click(cx, "hosts.save");
    let id = cx
        .update(|_window, app| view.read(app).book().hosts().first().map(HostConfig::id))
        .expect("saved");

    click(cx, "hosts.edit.0");
    click(cx, "hosts.remember");
    click(cx, "hosts.save");
    assert_eq!(
        cx.update(|_window, app| view.read(app).saved_secret_for_test(id)),
        None,
        "remembering was turned off, so the secret is gone"
    );

    click(cx, "hosts.edit.0");
    click(cx, "hosts.remember");
    type_into(cx, "hosts.field.secret", "second");
    click(cx, "hosts.save");
    assert_eq!(
        cx.update(|_window, app| view.read(app).saved_secret_for_test(id)),
        Some("second".to_owned())
    );

    click(cx, "hosts.delete.0");
    click(cx, "hosts.confirm.delete");
    assert_eq!(
        cx.update(|_window, app| view.read(app).saved_secret_for_test(id)),
        None,
        "a deleted host leaves no secret behind"
    );
}

#[gpui::test]
async fn the_dialog_shows_the_fields_of_the_chosen_sign_in_and_closes_on_escape(
    cx: &mut TestAppContext,
) {
    let (view, cx, _dir) = dialog_view(cx, Vec::new());
    click(cx, "hosts.new");
    click(cx, "hosts.auth.remote.method.agent");
    let auth = cx.update(|_window, app| {
        view.read(app)
            .hosts_state()
            .draft()
            .map(crate::app::hosts::HostDraft::auth)
    });
    assert_eq!(auth, Some(shellune_core::AuthMethodKind::Agent));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        !cx.update(|_window, app| view.read(app).hosts_state().is_editing()),
        "Escape closes the dialog without saving"
    );
    assert!(cx.update(|_window, app| view.read(app).book().hosts().is_empty()));
}

#[gpui::test]
async fn the_startup_command_is_typed_once_the_connection_is_live(cx: &mut TestAppContext) {
    let mut host = stored_host("web", "10.0.0.5", 22, "deploy", None);
    host.set_startup_command(Some("cd /srv && tmux attach"));
    let (view, cx, _dir) = dialog_view(cx, vec![host]);
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();

    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("the remote terminal is open");
    let sent = cx.update(|_window, app| pane.read(app).sent().to_vec());
    assert_eq!(sent, b"cd /srv && tmux attach\r".to_vec());
    assert_eq!(
        destination_of(&view, cx),
        Destination::LocalShells,
        "the terminal that was connected is shown in the workspace"
    );
    // The name is the tab's, and the head's second row carries the rest: the merge
    // leaves exactly one place for each (R12).
    assert!(
        cx.debug_bounds("session.tab.0").is_some(),
        "the connected session names itself on its tab"
    );
    assert!(
        cx.debug_bounds("session.context").is_some(),
        "the head describes the connected session"
    );
}

// ---------------------------------------------------------------------------
// One workspace for local and remote sessions (R1, R2, R3, R12)
// ---------------------------------------------------------------------------

#[gpui::test]
async fn local_and_remote_sessions_share_the_workspace(cx: &mut TestAppContext) {
    let (view, cx, _dir) =
        dialog_view(cx, vec![stored_host("web", "10.0.0.5", 22, "deploy", None)]);
    go(cx, crate::app::actions::GoToLocalShells);
    let local = start_local_session(cx, &view);

    click(cx, "nav.hosts");
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .workspace()
                .capture_snapshot()
                .expect("the context panel is part of the workspace snapshot")
                .panel()
                .section()
        }),
        shellune_core::PanelSection::Hosts
    );
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"remote ready\r\n$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();

    let remote = cx
        .update(|_window, app| view.read(app).active_session())
        .expect("the remote session is current");
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .workspace()
                .capture_snapshot()
                .expect("the context panel is part of the workspace snapshot")
                .panel()
                .section()
        }),
        shellune_core::PanelSection::None,
        "the connected session comes back to the workspace"
    );
    assert!(cx.debug_bounds("session.context").is_some());
    assert!(cx.debug_bounds("session.tab.0").is_some());
    assert!(
        cx.update(|_window, app| {
            view.read(app)
                .workspace()
                .session(remote)
                .is_some_and(|session| session.kind().is_remote())
        }),
        "the session on screen is the remote one"
    );

    if let Some(local) = local {
        click(cx, "session.tab.0");
        assert_eq!(destination_of(&view, cx), Destination::LocalShells);
        assert_eq!(
            cx.update(|_window, app| view.read(app).active_session()),
            Some(local)
        );
        assert!(cx.update(|_window, app| {
            view.read(app)
                .workspace()
                .session(local)
                .is_some_and(|session| !session.kind().is_remote())
        }));
        click(cx, "session.tab.1");
        assert_eq!(destination_of(&view, cx), Destination::LocalShells);
        assert_eq!(
            cx.update(|_window, app| view.read(app).active_session()),
            Some(remote)
        );
    }

    // The hosts home and the terminal tab of the session are one click apart.
    click(cx, "nav.hosts");
    assert_eq!(destination_of(&view, cx), Destination::Hosts);
    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    assert!(
        cx.debug_bounds("session.context").is_some(),
        "coming back shows which session is current"
    );
}

#[gpui::test]
async fn a_quick_connection_does_not_write_a_host_until_asked(cx: &mut TestAppContext) {
    let dir = TempDir::new("quick-unsaved");
    seed(&dir.store(), Vec::new());
    let file = dir.path().join(shellune_storage::FILE_NAME);
    let before = std::fs::read(&file).expect("the empty host file");
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "welcome.quick");
    assert!(cx.debug_bounds("quick.panel").is_some());
    type_into(cx, "quick.address", "127.0.0.1");
    clear_field(cx, "quick.port", 2);
    type_into(cx, "quick.port", "1");
    type_into(cx, "quick.user", "ada");
    type_into(cx, "quick.password", "s3cret-quick");
    click(cx, "quick.connect");

    let (hosts, pending) = cx.update(|_window, app| {
        let view = view.read(app);
        (view.book().hosts().len(), view.pending_attempt_count())
    });
    assert_eq!(hosts, 0, "connecting does not save a host");
    assert_eq!(pending, 1, "the attempt started without a saved host");
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    let after = std::fs::read(&file).expect("the host file is still there");
    assert_eq!(after, before, "the host file is untouched");
    assert!(
        !String::from_utf8_lossy(&after).contains("s3cret-quick"),
        "the password is not written"
    );

    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    go(cx, crate::app::actions::OpenPalette);
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
}

#[gpui::test]
async fn a_quick_connection_can_be_saved_as_a_host_afterwards(cx: &mut TestAppContext) {
    let dir = TempDir::new("quick-save");
    seed(&dir.store(), Vec::new());
    let file = dir.path().join(shellune_storage::FILE_NAME);
    let before = std::fs::read(&file).expect("the empty host file");
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "welcome.quick");
    type_into(cx, "quick.address", "203.0.113.10");
    type_into(cx, "quick.user", "ada");
    type_into(cx, "quick.password", "s3cret-quick");
    let prepared =
        cx.update(|_window, app| view.update(app, |view, cx| view.prepare_quick_connection(cx)));
    assert!(prepared, "the quick-connection fields are complete");
    assert!(
        cx.update(|_window, app| view.read(app).book().hosts().is_empty()),
        "preparing the form does not save the host"
    );

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();

    assert_eq!(
        std::fs::read(&file).expect("the host file"),
        before,
        "a live quick connection is still unsaved"
    );
    assert!(cx.debug_bounds("session.save_host").is_some());
    assert!(cx.debug_bounds("session.context").is_some());
    click(cx, "session.save_host");

    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    assert!(
        cx.debug_bounds("session.save.reason").is_some(),
        "saving states the result on the session bar"
    );
    assert!(
        cx.debug_bounds("session.save.next").is_some(),
        "saving states the next step on the session bar"
    );
    let (failed, reason, next) = cx
        .update(|_window, app| view.read(app).quick_save_report())
        .expect("the session bar keeps the save result");
    assert!(!failed);
    assert_eq!(reason, ENGLISH.quick_saved);
    assert_eq!(next, ENGLISH.quick_saved_next);

    let saved = cx.update(|_window, app| view.read(app).book().hosts().to_vec());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].address(), "203.0.113.10");
    assert_eq!(saved[0].user(), "ada");
    assert_eq!(
        cx.update(|_window, app| view.read(app).book().recent_hosts().len()),
        1,
        "saving records that the host was reached"
    );
    let written = std::fs::read_to_string(&file).expect("the host file was written");
    assert!(written.contains("203.0.113.10"));
    assert!(
        !written.contains("s3cret-quick"),
        "the password stays out of the host file"
    );
}

#[gpui::test]
async fn quick_connect_names_the_field_when_input_is_rejected(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "welcome.quick");
    type_into(cx, "quick.address", "example.com");
    type_into(cx, "quick.user", "ada");
    type_into(cx, "quick.password", "secret");
    clear_field(cx, "quick.port", 2);
    type_into(cx, "quick.port", "0");
    click(cx, "quick.connect");

    let reason = cx
        .update(|_window, app| view.read(app).quick_refusal().map(str::to_owned))
        .expect("a bad port is refused before dialling");
    assert_eq!(
        reason,
        format!(
            "{} (0). {}",
            ENGLISH.quick_port_invalid, ENGLISH.quick_fix_port
        )
    );
    assert_ne!(reason, ENGLISH.host_field_port);
    assert!(!reason.contains("outside the valid range"), "{reason}");
    assert!(cx.debug_bounds("quick.error").is_some());
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0
    );
    assert_eq!(
        cx.update(|_window, app| view.update(app, |view, cx| view.quick_address_text(cx))),
        "example.com",
        "the address that was typed stays in the form"
    );

    clear_field(cx, "quick.port", 1);
    type_into(cx, "quick.port", "abc");
    click(cx, "quick.connect");
    let reason = cx
        .update(|_window, app| view.read(app).quick_refusal().map(str::to_owned))
        .expect("an unparseable port is refused");
    assert_eq!(
        reason,
        format!(
            "{} (abc). {}",
            ENGLISH.quick_port_invalid, ENGLISH.quick_fix_port
        )
    );

    go(cx, ToggleLanguage);
    click(cx, "quick.connect");
    let reason = cx
        .update(|_window, app| view.read(app).quick_refusal().map(str::to_owned))
        .expect("the Chinese form explains the port");
    assert_eq!(
        reason,
        format!(
            "{} (abc). {}",
            CHINESE.quick_port_invalid, CHINESE.quick_fix_port
        )
    );
    assert_ne!(reason, CHINESE.host_field_port);
    assert!(
        reason
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    );
    assert!(!reason.contains("outside the valid range"), "{reason}");

    clear_field(cx, "quick.port", 3);
    type_into(cx, "quick.port", "22");
    clear_field(cx, "quick.user", 3);
    click(cx, "quick.connect");
    let reason = cx
        .update(|_window, app| view.read(app).quick_refusal().map(str::to_owned))
        .expect("an empty user is refused");
    assert_eq!(reason, CHINESE.quick_invalid_user);
    assert!(!reason.contains("user name cannot be empty"), "{reason}");
    assert_eq!(
        cx.update(|_window, app| view.update(app, |view, cx| view.quick_user_text(cx))),
        ""
    );

    go(cx, ToggleLanguage);
    click(cx, "quick.connect");
    let reason = cx
        .update(|_window, app| view.read(app).quick_refusal().map(str::to_owned))
        .expect("the English form explains the user field");
    assert_eq!(reason, ENGLISH.quick_invalid_user);
    assert!(!reason.contains("user name cannot be empty"), "{reason}");

    type_into(cx, "quick.user", "ada");
    clear_field(cx, "quick.address", "example.com".len());
    type_into(cx, "quick.address", "bad host");
    click(cx, "quick.connect");
    let reason = cx
        .update(|_window, app| view.read(app).quick_refusal().map(str::to_owned))
        .expect("a spaced address is refused");
    assert_eq!(reason, ENGLISH.quick_invalid_address_space);
    assert!(
        !reason.contains("host address cannot contain whitespace"),
        "{reason}"
    );

    go(cx, ToggleLanguage);
    click(cx, "quick.connect");
    let reason = cx
        .update(|_window, app| view.read(app).quick_refusal().map(str::to_owned))
        .expect("the Chinese form explains the address");
    assert_eq!(reason, CHINESE.quick_invalid_address_space);
    assert!(
        !reason.contains("host address cannot contain whitespace"),
        "{reason}"
    );
    assert_eq!(
        cx.update(|_window, app| view.update(app, |view, cx| view.quick_address_text(cx))),
        "bad host"
    );
}

#[gpui::test]
async fn quick_connect_dials_with_a_key_before_saving(cx: &mut TestAppContext) {
    let dir = TempDir::new("quick-key");
    seed(&dir.store(), Vec::new());
    let file = dir.path().join(shellune_storage::FILE_NAME);
    let before = std::fs::read(&file).expect("the empty host file");
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "welcome.quick");
    assert!(cx.debug_bounds("quick.method.password").is_some());
    assert!(cx.debug_bounds("quick.method.key").is_some());
    assert!(cx.debug_bounds("quick.method.key-passphrase").is_some());
    assert!(cx.debug_bounds("quick.method.agent").is_some());
    assert!(
        cx.debug_bounds("quick.key").is_none(),
        "a password method does not ask for a key path"
    );
    assert!(cx.debug_bounds("quick.passphrase").is_none());

    click(cx, "quick.method.key");
    assert!(cx.debug_bounds("quick.key").is_some());
    click(cx, "quick.method.key-passphrase");
    assert!(
        cx.debug_bounds("quick.passphrase").is_some(),
        "an encrypted key asks for its passphrase"
    );

    let key_path = "C:/keys/quick-only";
    type_into(cx, "quick.address", "127.0.0.1");
    clear_field(cx, "quick.port", 2);
    type_into(cx, "quick.port", "1");
    type_into(cx, "quick.user", "ada");
    type_into(cx, "quick.key", key_path);
    click(cx, "quick.connect");
    assert_eq!(
        cx.update(|_window, app| view.read(app).quick_refusal().map(str::to_owned)),
        Some(ENGLISH.auth_missing_passphrase.to_owned())
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0
    );

    click(cx, "quick.method.key");
    clear_field(cx, "quick.key", key_path.len());
    click(cx, "quick.connect");
    assert_eq!(
        cx.update(|_window, app| view.read(app).quick_refusal().map(str::to_owned)),
        Some(ENGLISH.auth_missing_key.to_owned())
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0,
        "a missing key path does not dial"
    );

    type_into(cx, "quick.key", key_path);
    click(cx, "quick.connect");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        1,
        "a key file dials without a password, an agent or a saved host"
    );
    assert!(cx.update(|_window, app| view.read(app).quick_refusal().is_none()));
    assert!(cx.update(|_window, app| view.read(app).book().hosts().is_empty()));
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    let after = std::fs::read(&file).expect("the host file is still there");
    assert_eq!(
        after, before,
        "connecting with a key does not save the host"
    );
    assert!(!String::from_utf8_lossy(&after).contains(key_path));
    cx.update(|_window, app| {
        let panel = view
            .read(app)
            .remote_state()
            .expect("the attempt keeps its form");
        assert_eq!(panel.method(), MethodChoice::KeyFile);
        assert_eq!(
            panel.host().auth(),
            Some(shellune_core::AuthMethodKind::KeyFile)
        );
        assert_eq!(panel.host().identity_file(), Some(key_path));
        assert!(!panel.host().remembers_secret());
    });
}

#[gpui::test]
async fn saving_a_quick_host_shows_a_write_failure_on_the_session_bar(cx: &mut TestAppContext) {
    let dir = TempDir::new("quick-save-fail");
    let blocked = dir.path().join("store-blocked");
    std::fs::write(&blocked, b"not-a-directory").expect("the blocked store path");
    let store = HostStore::at(&blocked);
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "welcome.quick");
    type_into(cx, "quick.address", "203.0.113.20");
    type_into(cx, "quick.user", "ada");
    type_into(cx, "quick.password", "s3cret-fail");
    let prepared =
        cx.update(|_window, app| view.update(app, |view, cx| view.prepare_quick_connection(cx)));
    assert!(prepared, "the quick-connection fields are complete");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::Connected,
                    shellune_ssh::RemoteEvent::Data(b"$ ".to_vec()),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();

    assert!(cx.debug_bounds("session.save_host").is_some());
    go(cx, ToggleLanguage);
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese
    );
    click(cx, "session.save_host");

    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    assert!(
        cx.debug_bounds("session.save.reason").is_some(),
        "a failed save states the reason on the session bar"
    );
    assert!(
        cx.debug_bounds("session.save.next").is_some(),
        "a failed save states the next step on the session bar"
    );
    assert!(
        cx.debug_bounds("session.save_host").is_some(),
        "the save button stays when the file was not written"
    );
    let (failed, reason, next) = cx
        .update(|_window, app| view.read(app).quick_save_report())
        .expect("the session bar keeps the failure");
    assert!(failed);
    assert!(
        reason.starts_with(CHINESE.quick_save_write_failed),
        "{reason}"
    );
    assert!(
        reason
            .chars()
            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    );
    assert!(!reason.contains("could not"), "{reason}");
    assert_eq!(next, CHINESE.quick_save_write_next);
    assert!(cx.update(|_window, app| view.read(app).book().hosts().is_empty()));
    assert_eq!(
        std::fs::read(&blocked).expect("the blocked path is unchanged"),
        b"not-a-directory"
    );
}

#[gpui::test]
async fn the_host_dialog_keeps_a_jump_and_a_proxy_command_on_the_form(cx: &mut TestAppContext) {
    let (view, cx, _dir) = dialog_view(
        cx,
        vec![
            stored_host("bastion", "10.0.0.1", 22, "jump", None),
            stored_host("web", "10.0.0.2", 22, "deploy", None),
        ],
    );
    click(cx, "hosts.edit.1");
    assert!(cx.debug_bounds("hosts.field.proxy").is_some());
    assert!(cx.debug_bounds("hosts.jump").is_some());
    click(cx, "hosts.jump.add.0");
    type_into(cx, "hosts.field.proxy", "nc %h %p");
    assert!(cx.debug_bounds("hosts.proxy.conflict").is_some());
    click(cx, "hosts.save");
    cx.update(|_window, app| {
        let host = view
            .read(app)
            .book()
            .hosts()
            .iter()
            .find(|host| host.name() == "web")
            .expect("web is saved")
            .clone();
        assert_eq!(host.jumps().len(), 1);
        assert_eq!(host.proxy_command(), Some("nc %h %p"));
    });
}

#[gpui::test]
async fn two_connection_forms_keep_their_own_secrets(cx: &mut TestAppContext) {
    let (view, cx, _dir) = dialog_view(
        cx,
        vec![
            stored_host("alpha", "10.0.0.5", 22, "deploy", None),
            stored_host("beta", "10.0.0.6", 22, "deploy", None),
        ],
    );
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", "alpha-secret");
    // The dialog covers the page, so the next host is opened the way the command
    // palette does it.
    open_saved_host(cx, &view, 1);
    type_into(cx, "remote.password", "beta-secret");
    assert!(cx.debug_bounds("attempt.others").is_some());
    click(cx, "attempt.row.0");
    let first = cx.update(|_window, app| {
        view.read(app)
            .remote_state()
            .expect("the first form is back")
            .password()
            .to_owned()
    });
    assert_eq!(first, "alpha-secret");
    click(cx, "attempt.row.1");
    let second = cx.update(|_window, app| {
        view.read(app)
            .remote_state()
            .expect("the second form is back")
            .password()
            .to_owned()
    });
    assert_eq!(second, "beta-secret");
    assert_ne!(first, second);
}

#[gpui::test]
async fn a_form_refused_before_dial_keeps_its_secret_and_its_failure(cx: &mut TestAppContext) {
    let bastion = stored_host("bastion", "10.0.0.1", 22, "jump", None);
    let mut broken = stored_host("broken", "10.0.0.7", 22, "deploy", None);
    broken.set_jumps([bastion.id()]);
    broken.set_proxy_command(Some("nc %h %p"));
    let (view, cx, _dir) = dialog_view(
        cx,
        vec![
            bastion,
            broken,
            stored_host("other", "10.0.0.8", 22, "deploy", None),
        ],
    );

    // Dialling is refused before an attempt exists. The form already has a session.
    click(cx, "hosts.connect.1");
    assert!(cx.debug_bounds("remote.failure").is_some());
    type_into(cx, "remote.password", "chain-secret");
    // The refused form fills the hosts page, so the next connect button sits under
    // the scroll. Opening the host is the same call that button makes.
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            let host = view
                .book()
                .hosts()
                .iter()
                .find(|host| host.name() == "other")
                .cloned()
                .expect("other");
            view.open_remote(&host, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("attempt.others").is_some(),
        "the refused form is parked instead of replaced"
    );
    click(cx, "attempt.row.0");
    let (password, reason) = cx.update(|_window, app| {
        let panel = view
            .read(app)
            .remote_state()
            .expect("the refused form is back");
        (
            panel.password().to_owned(),
            panel
                .status()
                .failure()
                .map(|failure| failure.reason().to_owned()),
        )
    });
    assert_eq!(password, "chain-secret");
    let reason = reason.expect("the refusal is still on the form");
    assert!(
        reason.contains("Nothing was connected"),
        "the dial-before failure is the one that was parked: {reason}"
    );
}

#[gpui::test]
async fn a_form_whose_attempt_was_dropped_keeps_its_secret_and_its_failure(
    cx: &mut TestAppContext,
) {
    let (view, cx, _dir) = dialog_view(
        cx,
        vec![
            stored_host("alpha", "10.0.0.5", 22, "deploy", None),
            stored_host("beta", "10.0.0.6", 22, "deploy", None),
        ],
    );

    // The transport ends and takes the attempt with it. The focused form stays,
    // bound to the session, with whatever was typed and the failure it showed.
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", "kept-secret");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![shellune_ssh::RemoteEvent::Failed(
                    shellune_ssh::ConnectError::AuthRejected,
                )],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();
    assert!(cx.debug_bounds("remote.failure").is_some());
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            let host = view
                .book()
                .hosts()
                .iter()
                .find(|host| host.name() == "beta")
                .cloned()
                .expect("beta");
            view.open_remote(&host, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("attempt.others").is_some());
    click(cx, "attempt.row.0");
    let (password, reason) = cx.update(|_window, app| {
        let panel = view
            .read(app)
            .remote_state()
            .expect("the ended form is back");
        (
            panel.password().to_owned(),
            panel
                .status()
                .failure()
                .map(|failure| failure.reason().to_owned()),
        )
    });
    assert_eq!(password, "kept-secret");
    let reason = reason.expect("the transport failure is still on the form");
    assert!(
        reason.contains("rejected"),
        "the failure from the dropped attempt is unchanged: {reason}"
    );
}

#[gpui::test]
async fn connection_attempts_wait_and_cancel_on_their_own(cx: &mut TestAppContext) {
    let (view, cx, _dir) = dialog_view(
        cx,
        vec![
            stored_host("alpha", "10.0.0.5", 22, "deploy", None),
            stored_host("beta", "10.0.0.6", 22, "deploy", None),
        ],
    );

    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.script_remote_attempt(Vec::new(), cx));
    });
    // The first attempt keeps running behind its tab while the next host is chosen.
    click(cx, "remote.hide");
    click(cx, "hosts.connect.1");
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.script_remote_attempt(Vec::new(), cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        2
    );
    assert!(cx.debug_bounds("attempt.others").is_some());
    assert!(cx.debug_bounds("session.tab.1").is_some());

    click(cx, "remote.hide");
    click(cx, "session.tab.close.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        1,
        "closing one attempt leaves the other waiting"
    );
    let waiting = cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    assert!(waiting, "the remaining attempt is still polled");

    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    go(cx, crate::app::actions::OpenPalette);
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    // The attempt that is left is a tab; its dialog comes back from there.
    click(cx, "session.tab.0");
    click(cx, "remote.close");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0
    );
}

#[gpui::test]
async fn one_attempt_failing_leaves_the_other_waiting(cx: &mut TestAppContext) {
    let (view, cx, _dir) = dialog_view(
        cx,
        vec![
            stored_host("alpha", "10.0.0.5", 22, "deploy", None),
            stored_host("beta", "10.0.0.6", 22, "deploy", None),
        ],
    );

    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![shellune_ssh::RemoteEvent::Failed(
                    shellune_ssh::ConnectError::AuthRejected,
                )],
                cx,
            );
        });
    });
    click(cx, "remote.hide");
    click(cx, "hosts.connect.1");
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.script_remote_attempt(Vec::new(), cx));
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();

    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        1,
        "only the attempt that failed has stopped"
    );
    let failed = cx.update(|_window, app| {
        view.read(app)
            .workspace()
            .sessions()
            .items()
            .iter()
            .any(|session| matches!(session.state(), SessionState::Failed(_)))
    });
    assert!(failed, "the failed attempt is recorded on its own session");
    assert!(cx.debug_bounds("attempt.others").is_some());
    assert!(
        cx.debug_bounds("remote.failure").is_none(),
        "the form on screen is the attempt that is still connecting"
    );

    click(cx, "attempt.row.0");
    assert!(cx.debug_bounds("remote.failure").is_some());
    let waiting = cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    assert!(waiting);
    click(cx, "attempt.cancel.1");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0
    );
    assert!(
        cx.debug_bounds("remote.failure").is_some(),
        "cancelling the other attempt leaves this failure on screen"
    );
    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    go(cx, crate::app::actions::OpenPalette);
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
}

#[gpui::test]
async fn a_failed_host_file_does_not_block_the_rest_of_the_window(cx: &mut TestAppContext) {
    let dir = TempDir::new("hosts-failed-nav");
    let root = dir.path().join("data");
    std::fs::write(&root, "not a directory").expect("the sentinel file is written");
    let store = HostStore::at(&root);
    let (view, cx) = cx
        .add_window_view(move |_window, cx| ShelluneView::with_store(cx, Workspace::new(), store));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let local = start_local_session(cx, &view);
    if let Some(local) = local {
        let sent = sent_after_typing(cx, &view, local, "while-hosts-failed");
        assert!(sent.contains("while-hosts-failed"), "{sent:?}");
    }

    click(cx, "nav.hosts");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().state(Destination::Hosts)),
        SectionState::Failed
    );
    assert!(cx.debug_bounds("state.card").is_some());

    go(cx, crate::app::actions::GoToLocalShells);
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    if let Some(local) = local {
        assert_eq!(
            session_state(cx, &view, local),
            Some(SessionState::Connected)
        );
        let sent = sent_after_typing(cx, &view, local, "still-typing");
        assert!(sent.contains("still-typing"), "{sent:?}");
    }
    go(cx, crate::app::actions::OpenPalette);
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().state(Destination::Hosts)),
        SectionState::Failed,
        "opening the palette does not clear the host failure"
    );
}

// ---------------------------------------------------------------------------
// Collapsing the sidebar
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The light palette
// ---------------------------------------------------------------------------

/// The light value of a dark token.
fn light(token: u32) -> u32 {
    theme::light_value(token)
}

#[test]
fn every_token_has_a_light_value() {
    let tokens = [
        palette::CANVAS,
        palette::SURFACE,
        palette::SURFACE_RAISED,
        palette::SURFACE_HOVER,
        palette::SURFACE_SELECTED,
        palette::BORDER,
        palette::BORDER_STRONG,
        palette::TEXT_PRIMARY,
        palette::TEXT_SECONDARY,
        palette::TEXT_MUTED,
        palette::ACCENT,
        palette::ACCENT_HOVER,
        palette::ON_ACCENT,
        palette::ACCENT_TINT,
        palette::ACCENT_ON_TINT,
        palette::POSITIVE_TINT,
        palette::WARNING_TINT,
        palette::DANGER_TINT,
        palette::POSITIVE,
        palette::WARNING,
        palette::DANGER,
        palette::PLANNED,
        palette::BRAND_END,
    ];
    for token in tokens
        .into_iter()
        .chain(
            palette::AVATARS
                .iter()
                .flat_map(|(tint, text)| [*tint, *text]),
        )
        .chain(palette::TAB_ACCENTS)
    {
        assert_ne!(light(token), token, "{token:06x} has no light value");
    }
}

#[test]
fn light_body_text_reaches_the_contrast_floor_on_every_surface() {
    let surfaces = [
        ("canvas", palette::CANVAS),
        ("surface", palette::SURFACE),
        ("raised", palette::SURFACE_RAISED),
        ("selected", palette::SURFACE_SELECTED),
    ];
    let text = [
        ("primary", palette::TEXT_PRIMARY),
        ("secondary", palette::TEXT_SECONDARY),
        ("muted", palette::TEXT_MUTED),
        ("accent", palette::ACCENT),
    ];
    for (surface_name, surface) in surfaces {
        for (text_name, token) in text {
            let ratio = theme::contrast::ratio(light(token), light(surface));
            assert!(
                ratio >= BODY_CONTRAST_FLOOR,
                "light {text_name} on {surface_name} is {ratio:.2}:1"
            );
        }
    }
}

#[test]
fn light_status_and_accent_pairs_are_readable() {
    let pairs = [
        ("positive", palette::POSITIVE, palette::POSITIVE_TINT),
        ("warning", palette::WARNING, palette::WARNING_TINT),
        ("danger", palette::DANGER, palette::DANGER_TINT),
        ("on accent", palette::ON_ACCENT, palette::ACCENT),
        (
            "accent on tint",
            palette::ACCENT_ON_TINT,
            palette::ACCENT_TINT,
        ),
    ];
    for (name, text, background) in pairs {
        let ratio = theme::contrast::ratio(light(text), light(background));
        assert!(ratio >= BODY_CONTRAST_FLOOR, "light {name} is {ratio:.2}:1");
    }
    for (tint, text) in palette::AVATARS {
        let ratio = theme::contrast::ratio(light(text), light(tint));
        assert!(
            ratio >= BODY_CONTRAST_FLOOR,
            "light avatar {text:06x} is {ratio:.2}:1"
        );
    }
}

#[test]
fn the_light_terminal_keeps_its_colours_readable() {
    let background = 0xfcfcfd;
    let lookup = |dark: u32| {
        theme::light::TERMINAL
            .iter()
            .find(|(from, _)| *from == dark)
            .map(|(_, to)| *to)
            .expect("a terminal colour has a light value")
    };
    assert_eq!(lookup(0x0b0c10), background);
    let foreground = lookup(0xe6e9f0);
    assert!(theme::contrast::ratio(foreground, background) >= BODY_CONTRAST_FLOOR);
    // Red, green, yellow, blue, magenta and cyan are text colours in every shell.
    for (index, color) in shellune_terminal::palette::ANSI[1..7].iter().enumerate() {
        let dark = u32::from(color.r) << 16 | u32::from(color.g) << 8 | u32::from(color.b);
        let ratio = theme::contrast::ratio(lookup(dark), background);
        assert!(
            ratio >= BODY_CONTRAST_FLOOR,
            "ANSI {} in the light terminal is {ratio:.2}:1",
            index + 1
        );
    }
}

#[test]
fn the_theme_choice_follows_the_system_until_one_is_picked() {
    use crate::app::theme::{Appearance, ThemeChoice};
    assert_eq!(ThemeChoice::System.appearance(true), Appearance::Dark);
    assert_eq!(ThemeChoice::System.appearance(false), Appearance::Light);
    assert_eq!(ThemeChoice::Light.appearance(true), Appearance::Light);
    assert_eq!(ThemeChoice::Dark.appearance(false), Appearance::Dark);
    assert_eq!(ThemeChoice::System.next(), ThemeChoice::Light);
    assert_eq!(ThemeChoice::Dark.next(), ThemeChoice::System);
}

#[gpui::test]
async fn the_theme_control_switches_the_palette(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.cycle_theme(cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).theme_choice()),
        theme::ThemeChoice::Light
    );
    assert_eq!(theme::appearance(), theme::Appearance::Light, "drawn light");
    assert_eq!(
        theme::swatch(palette::CANVAS),
        gpui::rgb(light(palette::CANVAS)),
        "a token resolves to its light value"
    );
    cx.update(|_window, app| view.update(app, |view, cx| view.cycle_theme(cx)));
    cx.run_until_parked();
    assert_eq!(theme::appearance(), theme::Appearance::Dark, "drawn dark");
    // Leave the thread as the other tests expect it.
    theme::set_appearance(theme::Appearance::Dark);
}

// ---------------------------------------------------------------------------
// The window's own title bar
// ---------------------------------------------------------------------------

#[test]
fn windows_and_macos_draw_their_own_title_bar_and_linux_keeps_the_system_one() {
    use crate::app::titlebar_for_test as custom_titlebar;
    assert!(custom_titlebar(Platform::Windows));
    assert!(custom_titlebar(Platform::MacOs));
    assert!(!custom_titlebar(Platform::Linux));
}

#[gpui::test]
async fn the_top_row_is_the_caption_and_keeps_its_buttons_inside_the_window(
    cx: &mut TestAppContext,
) {
    let (_view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("header.drag").is_some() && cx.debug_bounds("brand.drag").is_some(),
        "the blank parts of the top row are drag areas"
    );
    if Platform::current() == Platform::Windows {
        for button in ["window.minimize", "window.maximize", "window.close"] {
            assert_fits_inside(bounds_of(cx, button), minimum_window(), button);
        }
        assert_fits_inside(
            bounds_of(cx, "header.primary"),
            minimum_window(),
            "the main action beside the window buttons",
        );
    }
}

// ---------------------------------------------------------------------------
// Nested panes and a safe layout restore (R4)
// ---------------------------------------------------------------------------

fn sent_text(cx: &mut VisualTestContext, view: &Entity<ShelluneView>, id: SessionId) -> String {
    cx.update(|_window, app| {
        view.read(app)
            .pane_of(id)
            .map(|pane| String::from_utf8_lossy(pane.read(app).sent()).into_owned())
            .unwrap_or_default()
    })
}

#[gpui::test]
async fn nested_splits_keep_input_and_resize_on_the_active_pane(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }
    click(cx, "header.primary");
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.split_active_for_test(SplitAxis::LeftRight, window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.split_active_for_test(SplitAxis::TopBottom, window, cx);
        });
    });
    cx.run_until_parked();

    let leaves = cx.update(|_window, app| view.read(app).active_leaf_sessions_for_test());
    assert_eq!(leaves.len(), 3, "one sideways split and one stacked split");
    assert!(leaves.iter().all(|id| id.is_some()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs().len()),
        1,
        "the splits share one tab"
    );
    assert!(
        cx.debug_bounds("pane.active").is_some(),
        "the pane that receives input is marked"
    );
    let sessions: Vec<SessionId> = leaves.into_iter().flatten().collect();
    let (left, middle, bottom) = (sessions[0], sessions[1], sessions[2]);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(bottom),
        "the newest pane is the active one"
    );

    cx.simulate_input("only-bottom");
    cx.run_until_parked();
    assert!(
        sent_text(cx, &view, bottom).contains("only-bottom"),
        "input reached the active pane"
    );
    assert!(
        sent_text(cx, &view, left).is_empty() && sent_text(cx, &view, middle).is_empty(),
        "the other panes did not receive it"
    );

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.focus_direction_for_test(Direction::Left, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(left)
    );
    cx.simulate_input("only-left");
    cx.run_until_parked();
    assert!(sent_text(cx, &view, left).contains("only-left"));
    assert!(
        !sent_text(cx, &view, middle).contains("only-left")
            && !sent_text(cx, &view, bottom).contains("only-left"),
        "moving focus left the other panes untouched"
    );

    let ratios = cx.update(|_window, app| view.read(app).layout_ratios_for_test());
    assert_eq!(ratios.len(), 2);
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.resize_direction_for_test(Direction::Right, window, cx);
        });
    });
    let grown = cx.update(|_window, app| view.read(app).layout_ratios_for_test());
    assert_ne!(
        grown[0], ratios[0],
        "the sideways split around the active pane changed"
    );
    assert_eq!(grown[1], ratios[1], "the stacked split did not");

    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.focus_direction_for_test(Direction::Right, window, cx);
            view.focus_direction_for_test(Direction::Down, window, cx);
            view.resize_direction_for_test(Direction::Down, window, cx);
        });
    });
    let stacked = cx.update(|_window, app| view.read(app).layout_ratios_for_test());
    assert_eq!(stacked[0], grown[0], "the sideways split stayed");
    assert_ne!(
        stacked[1], grown[1],
        "only the stacked split around the active pane changed"
    );

    cx.update(|_window, app| {
        view.update(app, |view, cx| view.close_session(middle, cx));
    });
    cx.run_until_parked();
    let remaining = cx.update(|_window, app| view.read(app).active_leaf_sessions_for_test());
    assert_eq!(remaining, vec![Some(left), Some(bottom)]);
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_sessions().len()),
        2,
        "closing one pane left the other sessions running"
    );
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.focus_direction_for_test(Direction::Left, window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_input("still-left");
    cx.run_until_parked();
    assert!(
        sent_text(cx, &view, left).contains("still-left"),
        "the remaining pane still accepts input"
    );
    assert!(!sent_text(cx, &view, bottom).contains("still-left"));

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.close_session(bottom, cx);
            view.close_session(left, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).home_is_shown_for_test()),
        "the last pane leaves the hosts home, with its shells and servers"
    );
    assert!(
        cx.debug_bounds("shell.picker").is_some()
            || cx.debug_bounds("welcome.quick").is_some()
            || cx.debug_bounds("session.tab.new").is_some(),
        "the empty state names a next step"
    );
}

fn layout_script(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let marker = dir.join("restore-ran.txt");
    let script = if cfg!(windows) {
        let script = dir.join("should-not-run.cmd");
        std::fs::write(
            &script,
            format!("@echo off\r\necho leaked>\"{}\"\r\n", marker.display()),
        )
        .expect("script");
        script
    } else {
        let script = dir.join("should-not-run.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho leaked > \"{}\"\n", marker.display()),
        )
        .expect("script");
        script
    };
    (script, marker)
}

fn layout_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("port");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}

#[gpui::test]
async fn restoring_a_workspace_stays_pending_and_a_bad_host_is_local(cx: &mut TestAppContext) {
    let dir = TempDir::new("layout-restore");
    let (script, marker) = layout_script(&dir.path());
    let secret = "hunter2-layout-restore";
    let port = layout_port();
    let mut saved = stored_host("web", "10.0.0.5", 22, "deploy", None);
    saved.set_proxy_command(Some(script.display().to_string()));
    saved.set_startup_command(Some(secret));
    seed(&dir.store(), vec![saved.clone()]);
    let gone = shellune_core::HostId::new();
    let root = PaneNode::split(
        SplitAxis::LeftRight,
        5000,
        PaneNode::leaf(
            PaneTarget::local(Some(script.display().to_string())),
            TerminalMode::Classic,
        ),
        PaneNode::split(
            SplitAxis::TopBottom,
            4000,
            PaneNode::leaf(PaneTarget::remote(saved.id()), TerminalMode::Integrated),
            PaneNode::leaf(PaneTarget::remote(gone), TerminalMode::Classic),
        )
        .expect("inner"),
    )
    .expect("outer");
    let mut tab = WorkspaceTab::new("Work", root).expect("title");
    tab.set_active_pane(Some(shellune_core::PaneId::new()));
    let mut snapshot =
        WorkspaceSnapshot::new(vec![tab], PanelConfig::new(true, PanelSection::Files))
            .expect("snapshot");
    snapshot.set_active_tab(Some(shellune_core::TabId::new()));
    WorkspaceStore::at(dir.path())
        .save(&snapshot)
        .expect("snapshot saves");
    let mut rules = ForwardBook::new();
    rules
        .insert(ForwardRule::local(
            saved.id(),
            Endpoint::loopback(port).expect("listen"),
            Endpoint::new("db.internal", 5432).expect("target"),
        ))
        .expect("rule");
    ForwardStore::at(dir.path()).save(&rules).expect("rules");
    let before = std::fs::read(dir.path().join(WORKSPACE_FILE_NAME)).expect("file");
    assert!(
        !marker.exists(),
        "writing the snapshot must not run anything"
    );

    let workspace = Workspace::new();
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    assert_eq!(
        std::fs::read(dir.path().join(WORKSPACE_FILE_NAME)).expect("file"),
        before,
        "loading the layout must not rewrite it"
    );
    assert!(
        !marker.exists(),
        "restore must not run the shell or ProxyCommand"
    );
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("restore must not listen");
    assert_eq!(
        cx.update(|_window, app| view.read(app).forward_restore_state()),
        (1, false),
        "the saved rule is loaded and is not listening"
    );
    assert!(
        cx.update(|_window, app| view.read(app).open_sessions().is_empty()),
        "no terminal process was started"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0,
        "no connection was started"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        3
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).missing_host_panes_for_test()),
        vec![gone],
        "only the missing host is marked"
    );
    assert!(
        cx.update(|_window, app| view.read(app).layout_notice_count_for_test()) >= 2,
        "stale tab and pane ids stay as notices"
    );
    assert!(cx.debug_bounds("pane.pending").is_some());
    assert!(cx.debug_bounds("pane.missing").is_some());
    let saved_text = String::from_utf8_lossy(&before);
    assert!(
        !saved_text.contains(secret),
        "the snapshot does not keep the startup command"
    );
    assert!(
        !saved_text.to_lowercase().contains("password")
            && !saved_text.to_lowercase().contains("passphrase")
    );

    assert!(
        cx.update(|_window, app| { view.update(app, |view, cx| view.close_pending_for_test(cx)) }),
        "one pending pane can be removed"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        2,
        "the other panes stay"
    );
    assert!(!marker.exists());
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("removing a pane must not listen");
    let rewritten = std::fs::read(dir.path().join(WORKSPACE_FILE_NAME)).expect("file");
    assert_ne!(rewritten, before, "removing one pane is saved");
    assert!(!String::from_utf8_lossy(&rewritten).contains(secret));
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.close_all_sessions(cx));
    });
    assert_eq!(
        std::fs::read(dir.path().join(WORKSPACE_FILE_NAME)).expect("file"),
        rewritten,
        "closing the window does not replace the saved layout"
    );
}

#[gpui::test]
async fn a_named_tinted_tab_is_written_and_comes_back_waiting(cx: &mut TestAppContext) {
    let dir = TempDir::new("tab-identity-write");
    seed(&dir.store(), Vec::new());
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }
    click(cx, "header.primary");
    let session = cx
        .update(|_window, app| view.read(app).active_session())
        .expect("the shell opened a tab");

    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.rename");
    cx.simulate_input("build box");
    cx.run_until_parked();
    click(cx, "tab.rename.save");
    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.accent.violet");

    // The name and the colour are part of the saved layout, and the file keeps no
    // output, no command and no secret.
    let written = std::fs::read_to_string(dir.path().join(WORKSPACE_FILE_NAME))
        .expect("the layout was written");
    let document: serde_json::Value = serde_json::from_str(&written).expect("json");
    assert_eq!(document["tabs"][0]["custom_title"], "build box");
    assert_eq!(document["tabs"][0]["accent"], "violet");
    assert!(!written.to_lowercase().contains("password"), "{written}");
    assert!(!written.contains(&session.to_string()));

    // The next start restores the same labels as a waiting object: no shell, no
    // connection and no command runs.
    let saved = WorkspaceStore::at(dir.path())
        .load()
        .expect("the snapshot loads");
    let restored = saved.restore(|_| true);
    assert_eq!(restored.tabs()[0].custom_title(), Some("build box"));
    assert_eq!(
        restored.tabs()[0].accent(),
        Some(shellune_core::TabAccent::Violet)
    );
    assert!(
        restored
            .panes()
            .iter()
            .all(|pane| !pane.connection().is_started()),
        "the restored pane is waiting"
    );
}

#[gpui::test]
async fn a_restored_forward_is_listed_and_stays_unbound(cx: &mut TestAppContext) {
    let dir = TempDir::new("forward-visible");
    let port = layout_port();
    let exposed = layout_port();
    let saved = stored_host("web", "10.0.0.5", 22, "deploy", None);
    seed(&dir.store(), vec![saved.clone()]);
    let mut rules = ForwardBook::new();
    rules
        .insert(ForwardRule::local(
            saved.id(),
            Endpoint::loopback(port).expect("listen"),
            Endpoint::new("db.internal", 5432).expect("target"),
        ))
        .expect("rule");
    rules
        .insert(ForwardRule::local(
            saved.id(),
            Endpoint::new("0.0.0.0", exposed).expect("listen"),
            Endpoint::loopback(9).expect("target"),
        ))
        .expect("exposed");
    ForwardStore::at(dir.path()).save(&rules).expect("rules");
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    focus_root(&view, cx);
    cx.dispatch_action(GoToForwards);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).tool_for_test()),
        Some(crate::app::view::Tool::Forwards)
    );
    assert!(
        cx.debug_bounds("forwards.row.0").is_some(),
        "the saved rule is on the page"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).forward_restore_state()),
        (2, false)
    );
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("opening the page must not listen");
    let start = bounds_of(cx, "forwards.start.1");
    let center = start.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("forwards.exposure").is_some(),
        "a non-loopback listen asks before it binds"
    );
    std::net::TcpListener::bind(("127.0.0.1", exposed)).expect("confirming is required first");
}

#[gpui::test]
async fn closing_a_tab_or_removing_a_session_with_an_active_forward_asks_first(
    cx: &mut TestAppContext,
) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let host = shellune_core::HostId::new();
    let rule_a = ForwardRule::local(
        host,
        Endpoint::loopback(45111).expect("listen"),
        Endpoint::new("10.0.0.8", 80).expect("target"),
    );
    let rule_b = ForwardRule::local(
        host,
        Endpoint::loopback(45112).expect("listen"),
        Endpoint::new("10.0.0.9", 80).expect("target"),
    );
    let id_a = rule_a.id();
    let id_b = rule_b.id();
    let (mut observed_a, mut observed_b) = cx.update(|_window, app| {
        view.update(app, |view, _cx| {
            let observed_a =
                view.install_listening_forward_for_test(tabs[0], rule_a, "127.0.0.1", 45111);
            let observed_b =
                view.install_listening_forward_for_test(tabs[1], rule_b, "127.0.0.1", 45112);
            (observed_a, observed_b)
        })
    });
    let connected = |cx: &mut VisualTestContext, id: SessionId| {
        cx.update(|_window, app| {
            view.read(app)
                .workspace()
                .session(id)
                .is_some_and(|session| matches!(session.state(), SessionState::Connected))
        })
    };
    let listening = |cx: &mut VisualTestContext, id| {
        cx.update(|_window, app| view.read(app).forward_listening_for_test(id))
    };

    click(cx, "session.tab.close.0");
    assert!(
        cx.debug_bounds("session.close.question").is_some(),
        "closing the tab lists the forward before the connection stops"
    );
    assert!(cx.debug_bounds("session.close.forward.0").is_some());
    assert!(cx.debug_bounds("session.close.task.0").is_none());
    let lines = cx.update(|_window, app| view.read(app).closing_forward_lines_for_test());
    assert!(
        lines.iter().any(|line| line.contains("45111")),
        "the question names this connection's rule: {lines:?}"
    );
    assert!(
        lines.iter().all(|line| !line.contains("45112")),
        "the other connection's rule is not in this question: {lines:?}"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        tabs,
        "the tab stays until the question is answered"
    );
    assert!(connected(cx, tabs[0]) && connected(cx, tabs[1]));
    assert!(listening(cx, id_a) && listening(cx, id_b));
    assert!(observed_a.commands().is_empty() && observed_b.commands().is_empty());

    click(cx, "session.close.cancel");
    assert_eq!(
        cx.update(|_window, app| view.read(app).closing_for_test()),
        None
    );
    assert!(connected(cx, tabs[0]) && listening(cx, id_a));
    assert!(connected(cx, tabs[1]) && listening(cx, id_b));

    right_click(cx, "session.tab.1");
    click(cx, "tab.menu.close_others");
    assert_eq!(
        cx.update(|_window, app| view.read(app).closing_for_test()),
        Some(tabs[0]),
        "close other tabs asks before it stops the forward on the other tab"
    );
    assert_eq!(cx.update(|_window, app| view.read(app).open_tabs()), tabs);
    click(cx, "session.close.cancel");
    assert!(listening(cx, id_a) && listening(cx, id_b));

    right_click(cx, "session.tab.0");
    click(cx, "tab.menu.close_right");
    assert_eq!(
        cx.update(|_window, app| view.read(app).closing_for_test()),
        Some(tabs[1]),
        "close tabs to the right asks before it stops that forward"
    );
    assert_eq!(cx.update(|_window, app| view.read(app).open_tabs()), tabs);
    let lines = cx.update(|_window, app| view.read(app).closing_forward_lines_for_test());
    assert!(
        lines.iter().any(|line| line.contains("45112")),
        "the question names the rule on the tab to the right: {lines:?}"
    );
    click(cx, "session.close.cancel");

    cx.update(|_window, app| {
        view.update(app, |view, cx| view.remove_session(tabs[0], cx));
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).closing_for_test()),
        Some(tabs[0]),
        "removing the session asks before it drops the record"
    );
    assert!(
        cx.update(|_window, app| view.read(app).workspace().session(tabs[0]).is_some()),
        "the record stays so the question still has a subject"
    );
    assert!(connected(cx, tabs[0]) && listening(cx, id_a));
    assert_eq!(cx.update(|_window, app| view.read(app).open_tabs()), tabs);
    click(cx, "session.close.cancel");
    assert!(
        observed_a.commands().is_empty() && observed_b.commands().is_empty(),
        "cancelling does not stop either forward"
    );

    click(cx, "session.tab.close.0");
    click(cx, "session.close.confirm");
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        vec![tabs[1]],
        "confirming closes only the session that was asked about"
    );
    assert!(
        cx.update(|_window, app| {
            view.read(app)
                .workspace()
                .session(tabs[0])
                .is_some_and(|session| matches!(session.state(), SessionState::Closed))
        }),
        "the confirmed session is closed"
    );
    assert!(!listening(cx, id_a));
    assert_eq!(
        observed_a.commands(),
        vec![shellune_ssh::ForwardCommand::Stop { id: id_a }]
    );
    assert!(connected(cx, tabs[1]) && listening(cx, id_b));
    assert!(
        observed_b.commands().is_empty(),
        "the other session's forward keeps its channel"
    );
}

#[gpui::test]
async fn saving_a_listening_rule_onto_a_new_endpoint_withdraws_the_old_listener(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("forward-save-live");
    let port = layout_port();
    let saved = stored_host("web", "10.0.0.5", 22, "deploy", None);
    seed(&dir.store(), vec![saved.clone()]);
    let rule = ForwardRule::local(
        saved.id(),
        Endpoint::new("0.0.0.0", port).expect("listen"),
        Endpoint::loopback(9).expect("target"),
    );
    let id = rule.id();
    let mut rules = ForwardBook::new();
    rules.insert(rule.clone()).expect("rule");
    ForwardStore::at(dir.path()).save(&rules).expect("rules");
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    focus_root(&view, cx);
    let mut observed = cx.update(|_window, app| {
        view.update(app, |view, _cx| {
            view.install_listening_forward_for_test(
                shellune_core::SessionId::new(),
                rule,
                "0.0.0.0",
                port,
            )
        })
    });
    cx.dispatch_action(GoToForwards);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("forwards.listening.0").is_some(),
        "the row shows the bound listener before the edit"
    );

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.edit_forward_for_test(id, cx);
            view.save_forward_for_test(cx);
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).forward_listening_for_test(id)),
        "saving the same endpoints leaves the listener up"
    );
    assert!(observed.commands().is_empty());
    assert!(cx.debug_bounds("forwards.listening.0").is_some());

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.edit_forward_for_test(id, cx);
            view.set_forward_listen_for_test("127.0.0.1", &port.to_string(), cx);
            view.save_forward_for_test(cx);
        });
    });
    cx.run_until_parked();
    let summary = cx
        .update(|_window, app| view.read(app).forward_row_summary_for_test(0))
        .expect("the row is still saved");
    assert!(
        !summary.contains("Listening"),
        "the new endpoint is not marked listening: {summary}"
    );
    assert!(
        summary.contains("Stopped") && summary.contains(&format!("127.0.0.1:{port}")),
        "the row shows the saved endpoint as stopped, not as the old bind: {summary}"
    );
    assert!(!cx.update(|_window, app| view.read(app).forward_listening_for_test(id)));
    assert_eq!(
        observed.commands(),
        vec![shellune_ssh::ForwardCommand::Stop { id }]
    );
    let stored = ForwardStore::at(dir.path()).load().expect("rules");
    assert_eq!(stored.rule(id).expect("rule").listen().host(), "127.0.0.1");
    std::net::TcpListener::bind(("127.0.0.1", port)).expect("the new endpoint is not listening");

    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.edit_forward_for_test(id, cx);
            view.set_forward_listen_for_test("0.0.0.0", &port.to_string(), cx);
            view.save_forward_for_test(cx);
        });
    });
    cx.run_until_parked();
    click(cx, "forwards.start.0");
    assert!(
        cx.debug_bounds("forwards.exposure").is_some(),
        "the next real start of a non-loopback listen still asks"
    );
    assert!(!cx.update(|_window, app| view.read(app).forward_listening_for_test(id)));
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0,
        "the confirmation has not started a connection"
    );
}

#[gpui::test]
async fn a_forward_whose_host_was_deleted_explains_that_on_its_row(cx: &mut TestAppContext) {
    let dir = TempDir::new("forward-missing-host");
    let missing = shellune_core::HostId::new();
    let port = layout_port();
    let rule = ForwardRule::local(
        missing,
        Endpoint::loopback(port).expect("listen"),
        Endpoint::new("db.internal", 5432).expect("target"),
    );
    let id = rule.id();
    let mut rules = ForwardBook::new();
    rules.insert(rule).expect("rule");
    ForwardStore::at(dir.path()).save(&rules).expect("rules");
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    focus_root(&view, cx);
    cx.dispatch_action(GoToForwards);
    cx.run_until_parked();

    assert!(cx.debug_bounds("forwards.row.0").is_some());
    assert!(cx.debug_bounds("forwards.form").is_none());
    assert!(
        cx.debug_bounds("forwards.fault.0").is_some(),
        "the row itself says why this saved rule cannot start"
    );
    let fault = cx
        .update(|_window, app| view.read(app).forward_fault_for_test(0))
        .expect("the missing host is a fault");
    assert!(
        fault.contains(&missing.to_string()) && fault.contains(&id.to_string()),
        "the fault names the rule and the host: {fault}"
    );
    assert!(
        fault.contains("not saved") && fault.contains("not started"),
        "the fault says the host is gone and nothing was started: {fault}"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0
    );
    assert!(!cx.update(|_window, app| view.read(app).forward_listening_for_test(id)));

    click(cx, "forwards.start.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0,
        "starting does not dial a host that is not saved"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).tool_for_test()),
        Some(crate::app::view::Tool::Forwards)
    );
    assert!(cx.debug_bounds("forwards.exposure").is_none());
    assert!(cx.debug_bounds("forwards.fault.0").is_some());
    assert!(!cx.update(|_window, app| view.read(app).forward_listening_for_test(id)));
    let summary = cx
        .update(|_window, app| view.read(app).forward_row_summary_for_test(0))
        .expect("row");
    assert!(
        !summary.contains("Listening"),
        "a saved rule with no host is not a listener: {summary}"
    );
}

#[gpui::test]
async fn an_empty_snapshot_does_not_replace_the_open_page(cx: &mut TestAppContext) {
    let dir = TempDir::new("empty-layout");
    let mut workspace = Workspace::new();
    workspace.select(Destination::Hosts);
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Hosts);
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        0
    );
    assert!(cx.update(|_window, app| view.read(app).layout_error_for_test().is_none()));
}

#[gpui::test]
async fn a_damaged_snapshot_is_left_unchanged(cx: &mut TestAppContext) {
    let dir = TempDir::new("damaged-layout");
    let path = dir.path().join(WORKSPACE_FILE_NAME);
    std::fs::write(&path, b"{not-json").expect("write");
    let before = std::fs::read(&path).expect("read");
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, Workspace::new(), store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    assert_eq!(std::fs::read(&path).expect("read"), before);
    assert!(cx.update(|_window, app| view.read(app).layout_error_for_test().is_some()));
    assert!(cx.update(|_window, app| view.read(app).open_sessions().is_empty()));
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        0
    );
}

// ---------------------------------------------------------------------------
// Pane chrome, empty pages and waiting panes
// ---------------------------------------------------------------------------

#[gpui::test]
async fn a_single_pane_has_no_frame_and_a_split_marks_each_pane(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }
    click(cx, "header.primary");
    assert!(
        cx.debug_bounds("pane.close").is_none(),
        "a lone pane is the session: no pane header repeats it"
    );
    assert!(
        cx.debug_bounds("pane.split.right").is_some()
            && cx.debug_bounds("pane.split.down").is_some(),
        "the split buttons are in the head, so no pane needs a strip for them"
    );
    click(cx, "pane.split.right");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_leaf_sessions_for_test())
            .len(),
        2
    );
    assert!(
        cx.debug_bounds("pane.close").is_some(),
        "the active pane keeps its own close beside its own mark"
    );
    assert!(cx.debug_bounds("pane.active").is_some());
    // R12: a split adds which pane is which, not a second copy of the session's name.
    assert!(
        cx.debug_bounds("pane.active.mark").is_some(),
        "the active pane is marked in the pane, not only in the head"
    );
    assert!(
        cx.debug_bounds("pane.idle").is_some(),
        "the pane the keys are not in is drawn, and marked apart"
    );
}

#[gpui::test]
async fn the_head_is_one_band_that_names_the_session_once(cx: &mut TestAppContext) {
    // R12: the strip and the session's row under it are one continuous head, and the
    // name lives in one place. Before the merge the tab wrote "Zsh" and the bar under
    // it wrote "Zsh" again, with the same close button beside both.
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let Some(_id) = start_local_session(cx, &view) else {
        eprintln!("no shell on this machine, skipping");
        return;
    };

    let header = bounds_of(cx, "session.header");
    let tabs = bounds_of(cx, "session.tabs");
    let context = bounds_of(cx, "session.context");
    assert_fits_inside(tabs, header.size, "the tab strip in the head");
    assert_fits_inside(context, header.size, "the session's own row in the head");
    assert!(
        context.origin.y >= tabs.origin.y + tabs.size.height - px(1.0),
        "the session row is under the strip, in the same band: {context:?} vs {tabs:?}"
    );
    // The name is the tab's. Node that repeated it below is gone.
    assert!(
        cx.debug_bounds("session.tab.0").is_some(),
        "the tab is where the session names itself"
    );
    assert!(
        cx.debug_bounds("session.identity").is_none(),
        "the name is not drawn a second time under the tabs"
    );
    // The terminal starts where the head ends: no second band, no gap.
    let terminal = bounds_of(cx, "terminal.viewport");
    assert!(
        terminal.origin.y >= header.origin.y + header.size.height - px(1.0),
        "the terminal is drawn over the head or beside it: {terminal:?} vs {header:?}"
    );
    assert!(
        terminal.origin.y - (header.origin.y + header.size.height) < px(16.0),
        "a band was left between the head and the terminal: {terminal:?} vs {header:?}"
    );
    // One close button for the session, not one per strip.
    assert!(cx.debug_bounds("session.close").is_some());
    assert_fits_inside(
        bounds_of(cx, "session.close"),
        header.size,
        "the session's close button in the head",
    );
}

#[gpui::test]
async fn the_head_menu_holds_the_actions_that_do_not_need_a_button(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let Some(_id) = start_local_session(cx, &view) else {
        return;
    };
    // The head keeps one button for the actions a user needs at a glance and one menu
    // for the rest (R5).
    assert!(cx.debug_bounds("session.more").is_some());
    assert!(
        cx.debug_bounds("session.more.menu").is_none(),
        "the menu is closed until it is asked for"
    );

    click(cx, "session.more");
    assert!(
        cx.debug_bounds("session.more.menu").is_some(),
        "the menu opens on the head's own button"
    );
    assert!(cx.debug_bounds("session.more.duplicate").is_some());
    assert!(cx.debug_bounds("session.more.close").is_some());

    // Escape closes it and gives the keys back to the program in the terminal, which
    // is where they were before the menu opened (R5).
    press_key(cx, "escape");
    assert!(
        !cx.update(|_window, app| view.read(app).session_more_open()),
        "Escape closes the menu, even while the terminal holds the keys"
    );
    cx.run_until_parked();
    assert!(
        cx.update(|window, app| view.read(app).terminal_has_the_keyboard(window, app)),
        "closing the menu hands the keys back to the terminal"
    );
}
#[gpui::test]
async fn start_all_starts_every_waiting_local_pane(cx: &mut TestAppContext) {
    let dir = TempDir::new("start-all");
    seed(&dir.store(), Vec::new());
    let root = PaneNode::split(
        SplitAxis::LeftRight,
        5000,
        PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic),
        PaneNode::leaf(PaneTarget::local(None), TerminalMode::Classic),
    )
    .expect("split");
    let tab = WorkspaceTab::new("Work", root).expect("title");
    let snapshot = WorkspaceSnapshot::new(vec![tab], PanelConfig::new(false, PanelSection::None))
        .expect("snapshot");
    WorkspaceStore::at(dir.path())
        .save(&snapshot)
        .expect("snapshot saves");

    let workspace = Workspace::new();
    let store = HostStore::at(dir.path());
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        2
    );
    click(cx, "pane.connect_all");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_pane_count_for_test()),
        0,
        "both panes started"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_sessions().len()),
        2
    );
}

// ---------------------------------------------------------------------------
// The redesigned shell: caption, settings and port forwarding
// ---------------------------------------------------------------------------

#[gpui::test]
async fn a_press_on_the_caption_does_not_take_the_keyboard(cx: &mut TestAppContext) {
    // The caption is for moving the window. A press there must not move the keyboard
    // out of the terminal, or the next keystroke would go nowhere.
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return;
    }
    click(cx, "header.primary");
    let pane = cx
        .update(|_window, app| view.read(app).pane().cloned())
        .expect("a terminal is open");
    assert!(cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)));

    let caption = bounds_of(cx, "header.drag").center();
    cx.simulate_mouse_move(caption, None, Modifiers::default());
    cx.simulate_mouse_down(caption, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(caption, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.update(|window, app| pane.read(app).focus_handle().is_focused(window)),
        "the caption leaves the keyboard where it was"
    );
}

#[gpui::test]
async fn the_settings_page_changes_theme_language_and_font_size(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    assert_eq!(destination_of(&view, cx), Destination::Settings);

    click(cx, "settings.theme.2");
    assert_eq!(
        cx.update(|_window, app| view.read(app).theme_choice()),
        theme::ThemeChoice::Dark
    );
    click(cx, "settings.item.language");
    click(cx, "settings.language.1");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese
    );
    click(cx, "settings.item.text-size");
    click(cx, "settings.zoom.2");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Comfortable
    );
    click(cx, "settings.category.terminal");
    click(cx, "settings.item.font-size");
    let before = cx.update(|_window, app| view.read(app).workspace().local_font_size());
    click(cx, "settings.font.more");
    let after = cx.update(|_window, app| view.read(app).workspace().local_font_size());
    assert!(after > before, "A+ makes the terminal font larger");
}

#[gpui::test]
async fn the_tunnel_form_names_its_fields_and_says_what_it_will_do(cx: &mut TestAppContext) {
    let dir = TempDir::new("tunnel-form");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let (view, cx) = hosts_view(cx, dir.store());
    click(cx, "hosts.forwards.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).tool_for_test()),
        Some(crate::app::view::Tool::Forwards)
    );
    click(cx, "forwards.new");
    assert!(cx.debug_bounds("forwards.form").is_some());
    assert!(cx.debug_bounds("forwards.listen").is_some());
    assert!(cx.debug_bounds("forwards.target").is_some());
    assert!(
        cx.debug_bounds("forwards.preview").is_some(),
        "the form says in one sentence what the tunnel will do"
    );

    // A SOCKS proxy has no fixed destination, so its target fields go away.
    click(cx, "forwards.kind.2");
    assert_eq!(
        cx.update(|_window, app| view.read(app).forward_editor_kind_for_test()),
        Some(shellune_core::ForwardKind::Dynamic)
    );
}

#[gpui::test]
async fn a_right_click_in_the_host_dialog_opens_the_field_menu(cx: &mut TestAppContext) {
    let (_view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    click(cx, "hosts.new");
    right_click(cx, "hosts.field.address");
    assert!(
        cx.debug_bounds("field.menu").is_some(),
        "a right click opens the field's menu"
    );
}

#[gpui::test]
async fn a_right_click_in_the_palette_opens_the_field_menu(cx: &mut TestAppContext) {
    // The palette floats over the window too; its search field's menu must open
    // on top of it rather than take the window down.
    let (_view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    go(cx, crate::app::actions::OpenPalette);
    right_click(cx, "palette.field");
    assert!(cx.debug_bounds("field.menu").is_some());
}

#[gpui::test]
async fn the_new_tab_menu_hangs_from_its_control(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    let control = bounds_of(cx, "shell.menu");
    click(cx, "shell.menu");
    let menu = bounds_of(cx, "shell.picker");
    assert!(
        menu.top() >= control.bottom() && menu.top() - control.bottom() < px(16.0),
        "the menu opens just under the control: {control:?} {menu:?}"
    );
    assert!(
        menu.left() <= control.right() && menu.right() >= control.left(),
        "the menu lines up with the control"
    );

    // The custom path is a dialog of its own, not a field squeezed into the menu.
    click(cx, "shell.custom");
    assert!(!cx.update(|_window, app| view.read(app).shell_menu_open()));
    assert!(cx.debug_bounds("shell.custom.dialog").is_some());
    right_click(cx, "shell.path");
    assert!(
        cx.debug_bounds("field.menu").is_some(),
        "its field has a menu too"
    );
}

#[gpui::test]
async fn frame_lines_and_wide_characters_keep_the_columns_after_them(cx: &mut TestAppContext) {
    // A frame line, a CJK character and an icon come from other faces at their own
    // widths. Each is drawn in its own cells, so the text after them still starts
    // on its column.
    let (pane, cx) = pane_with("│中─x\r\n", cx);
    let cell = cx.update(|_window, app| pane.read(app).cell_size());
    let row = bounds_of(cx, "terminal.row.0");
    let wide = bounds_of(cx, "terminal.text.0.1");
    let after = bounds_of(cx, "terminal.text.0.4");
    // Positions are rounded to whole pixels when laid out, so half a pixel is
    // the tolerance.
    let at = |bounds: gpui::Bounds<Pixels>, column: f32| {
        (bounds.left() - row.left() - px(column * cell.width)).abs() <= px(0.5)
    };
    assert!(
        at(wide, 1.0),
        "the wide character sits on column 1: {wide:?}"
    );
    assert!(
        (wide.size.width - px(cell.width * 2.0)).abs() <= px(0.5),
        "and fills its two cells: {wide:?}"
    );
    assert!(
        at(after, 4.0),
        "the x after the rule is on column 4: {after:?}"
    );
}

#[gpui::test]
async fn a_program_that_asks_for_the_mouse_receives_clicks_and_the_wheel(cx: &mut TestAppContext) {
    // htop, vim, tmux: DECSET 1000 + 1006 asks for clicks in SGR form.
    let (pane, cx) = pane_with("\x1b[?1000h\x1b[?1006h", cx);
    let origin = bounds_of(cx, "terminal.row.0").origin;
    let cell = cx.update(|_window, app| pane.read(app).cell_size());
    let at = point(
        origin.x + px(cell.width * 2.5),
        origin.y + px(cell.height * 1.5),
    );
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Lines(point(0.0, 1.0)),
        ..Default::default()
    });
    cx.run_until_parked();
    let sent =
        cx.update(|_window, app| String::from_utf8_lossy(pane.read(app).sent()).into_owned());
    assert!(
        sent.contains("\x1b[<0;3;2M"),
        "the press names column 3, row 2: {sent:?}"
    );
    assert!(sent.contains("\x1b[<0;3;2m"), "and the release: {sent:?}");
    assert!(
        sent.contains("\x1b[<64;3;2M"),
        "the wheel is a button press: {sent:?}"
    );
}

#[gpui::test]
async fn the_wheel_scrolls_a_full_screen_program_that_did_not_ask_for_the_mouse(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("\x1b[?1049h", cx);
    let at = bounds_of(cx, "terminal.row.0").center();
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Lines(point(0.0, -2.0)),
        ..Default::default()
    });
    cx.run_until_parked();
    let sent =
        cx.update(|_window, app| String::from_utf8_lossy(pane.read(app).sent()).into_owned());
    assert_eq!(sent, "\x1b[B\x1b[B", "two lines down are two cursor-downs");
}

#[gpui::test]
async fn a_right_click_in_the_terminal_offers_copy_and_paste(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("hello\r\n", cx);
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("pasted".to_owned()));
    right_click(cx, "terminal.row.0");
    assert!(cx.debug_bounds("terminal.menu").is_some(), "the menu opens");
    click(cx, "terminal.menu.paste");
    let sent =
        cx.update(|_window, app| String::from_utf8_lossy(pane.read(app).sent()).into_owned());
    assert!(
        sent.contains("pasted"),
        "the clipboard went to the shell: {sent:?}"
    );
}

#[gpui::test]
async fn an_image_pasted_into_a_local_terminal_is_left_to_the_program(cx: &mut TestAppContext) {
    // Command-line tools that take images read the clipboard themselves on Ctrl+V.
    let (pane, cx) = pane_with("", cx);
    cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        vec![0x89, b'P', b'N', b'G'],
    )));
    cx.simulate_keystrokes("ctrl-shift-v");
    cx.run_until_parked();
    let sent = cx.update(|_window, app| pane.read(app).sent().to_vec());
    assert_eq!(sent, vec![0x16], "the program is sent Ctrl+V");
}

#[gpui::test]
async fn a_right_click_on_a_file_offers_its_actions_and_copies_its_path(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let _answers = open_files(cx, &view, id);
    // Row 1 is notes.txt, a file on the server.
    right_click(cx, "files.row.1");
    assert!(
        cx.debug_bounds("files.menu").is_some(),
        "the menu opens on the item"
    );
    assert!(cx.debug_bounds("files.menu.to_local").is_some());
    assert!(cx.debug_bounds("files.menu.delete").is_some());
    click(cx, "files.menu.copy_path");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("/home/deploy/notes.txt".to_owned())
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).toast_titles()),
        vec![crate::app::copy::ux().toast_path_copied.to_owned()],
        "the copy is confirmed in the corner"
    );
}

// ---------------------------------------------------------------------------
// Configurable shortcuts, rebound and unbound (R5)
// ---------------------------------------------------------------------------

/// A saved set for whichever platform the test runs on.
fn saved_for_this_platform(entries: Vec<shellune_storage::KeymapEntry>) -> KeymapPreferences {
    let mut saved = KeymapPreferences::default();
    saved.set_for_platform(Platform::current().id(), entries);
    saved
}

#[gpui::test]
async fn the_settings_page_lists_every_action_and_narrows_them_by_search(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    click(cx, "settings.category.keys");

    assert!(
        cx.debug_bounds("settings.keys").is_some(),
        "the settings page has a shortcut group"
    );
    assert!(cx.debug_bounds("settings.keys.row.palette.open").is_some());
    assert!(cx.debug_bounds("settings.keys.row.terminal.copy").is_some());
    assert!(
        cx.debug_bounds("settings.keys.change.palette.open")
            .is_some()
    );
    assert!(
        cx.debug_bounds("settings.keys.unbind.palette.open")
            .is_some()
    );
    assert!(cx.debug_bounds("settings.keys.count").is_some());

    // The search narrows the list: a phrase nothing matches brings the empty state,
    // which is not drawn while every action is listed.
    type_into(cx, "settings.keys.search", "zzq");
    assert_eq!(
        cx.update(|_window, app| view.read(app).keys_query(app)),
        "zzq",
        "the field took the query"
    );
    assert!(cx.debug_bounds("settings.keys.empty").is_some());
}

#[gpui::test]
async fn changing_a_shortcut_applies_at_once_and_the_palette_shows_it(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    click(cx, "settings.category.keys");
    click(cx, "settings.keys.change.nav.settings");
    assert!(
        cx.debug_bounds("keys.capture").is_some(),
        "the capture layer opens"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).capturing_for()),
        Some("nav.settings")
    );

    cx.simulate_keystrokes("ctrl-alt-s");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).capturing_for()),
        None,
        "a usable combination closes the layer"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .keymap()
            .keystroke("nav.settings")
            .map(str::to_owned)),
        Some("ctrl-alt-s".to_owned())
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).keymap().label("nav.settings")),
        Some(crate::app::keymap::keystroke_label(
            "ctrl-alt-s",
            Platform::current()
        ))
    );
    // The command palette reads the same configuration, so its hint changed too.
    assert_eq!(
        cx.update(|_window, app| view.read(app).palette_shortcut(copy::ux().settings_title)),
        Some(crate::app::keymap::keystroke_label(
            "ctrl-alt-s",
            Platform::current()
        )),
        "the palette shows the binding in force"
    );

    // The old combination no longer goes to the settings, the new one does.
    click(cx, "nav.hosts");
    let old = Keymap::defaults(Platform::current())
        .keystroke("nav.settings")
        .expect("the default settings shortcut")
        .to_owned();
    cx.simulate_keystrokes(&old);
    cx.run_until_parked();
    assert_eq!(
        destination_of(&view, cx),
        Destination::Hosts,
        "the replaced combination no longer reaches the settings"
    );
    cx.simulate_keystrokes("ctrl-alt-s");
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Settings);
}

#[gpui::test]
async fn a_conflicting_shortcut_is_refused_and_names_both_actions(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    click(cx, "settings.category.keys");
    click(cx, "settings.keys.change.nav.hosts");

    // The combination that already closes the current tab, in the same context.
    let taken = Keymap::defaults(Platform::current())
        .keystroke("tab.close")
        .expect("closing a tab has a default shortcut")
        .to_owned();
    cx.simulate_keystrokes(&taken);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).capturing_for()),
        Some("nav.hosts"),
        "the layer stays open so the answer can be corrected"
    );
    assert!(
        cx.debug_bounds("keys.capture.error").is_some(),
        "the conflict is shown instead of being saved"
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .keymap()
            .keystroke("nav.hosts")
            .map(str::to_owned)),
        Keymap::defaults(Platform::current())
            .keystroke("nav.hosts")
            .map(str::to_owned),
        "the refused change did not touch the binding"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).capturing_for()),
        None
    );
}

#[gpui::test]
async fn a_saved_keymap_that_cannot_be_used_is_reported_and_the_defaults_answer(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    let saved = saved_for_this_platform(vec![shellune_storage::KeymapEntry::new(
        "future.action",
        "shell",
        Some("ctrl-alt-k".to_owned()),
    )]);
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.use_saved_keymap_for_test(&saved, cx);
        });
    });
    cx.run_until_parked();

    click(cx, "settings.category.keys");
    assert!(
        cx.update(|_window, app| view.read(app).keymap_problem().is_some()),
        "the reason is kept for the page to show"
    );
    assert!(
        cx.debug_bounds("settings.keys.banner").is_some(),
        "the settings page says the saved shortcuts were not applied"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).keymap().clone()),
        Keymap::defaults(Platform::current()),
        "the defaults are in force, so navigation still works"
    );
}

#[gpui::test]
async fn unbinding_a_terminal_shortcut_hands_the_key_back_to_the_program(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    // Paste on Ctrl+Shift+V on every platform, so the unbound combination is one a
    // shell reads (Ctrl+V) rather than a platform key the terminal does not send.
    let mut keymap = Keymap::defaults(Platform::current());
    keymap
        .set("terminal.paste", "ctrl-shift-v")
        .expect("a free combination");
    cx.update(|_window, app| crate::app::apply_keymap(app, &keymap));
    cx.write_to_clipboard(ClipboardItem::new_string("pasted".to_owned()));

    cx.simulate_keystrokes("ctrl-shift-v");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"pasted".to_vec(),
        "with the action bound, the combination pastes"
    );

    // Unbind it: the same combination is now the program's own key.
    keymap.unbind("terminal.paste");
    cx.update(|_window, app| crate::app::apply_keymap(app, &keymap));
    cx.simulate_keystrokes("ctrl-shift-v");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"pasted\x16".to_vec(),
        "an unbound shortcut reaches the shell as the program's key"
    );
}

#[gpui::test]
async fn icon_controls_keep_a_24_by_24_pointer_target_at_the_smallest_text_size(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(minimum_window());
    smallest_text_size(&view, cx);
    click(cx, "nav.settings");
    click(cx, "settings.category.terminal");
    click(cx, "settings.item.font-size");
    for selector in [
        "settings.font.less",
        "settings.font.more",
        "palette.open",
        // The header's split "new shell / pick a shell" control: the chevron is a
        // target of its own, not a 18px drawing.
        "shell.menu",
    ] {
        assert_pointer_target(cx, selector);
    }

    // The terminal's own controls follow the same rule.
    let (_pane, cx) = pane_with("", cx);
    for selector in [
        "terminal.mode",
        "terminal.search",
        "terminal.copy",
        "terminal.font.increase",
        "terminal.font.decrease",
    ] {
        assert_pointer_target(cx, selector);
    }
}

/// Sets the interface text size to the smallest step, the tightest metrics ever get.
fn smallest_text_size(view: &Entity<ShelluneView>, cx: &mut VisualTestContext) {
    focus_root(view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Compact {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Compact
    );
}

/// Sets the interface text size to the largest step, where labels and controls are at
/// their widest against the window.
fn largest_text_size(view: &Entity<ShelluneView>, cx: &mut VisualTestContext) {
    focus_root(view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest
    );
}

fn assert_pointer_target(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = bounds_of(cx, selector);
    assert!(
        bounds.size.width >= px(24.0) && bounds.size.height >= px(24.0),
        "{selector} is a pointer target smaller than 24x24: {bounds:?}"
    );
}

/// One file row's four actions, measured against the row they belong to.
///
/// Each is a 24×24 target of its own, they sit right of the entry column (which carries
/// the size) in the order they are drawn, and the last one stays inside the row: a
/// column that is too narrow would push them over the entry, and a target covers its
/// neighbour as soon as it is enlarged without room (R4).
fn assert_file_actions_keep_their_column(cx: &mut VisualTestContext) {
    let row = bounds_of(cx, "files.row.1");
    let entry = bounds_of(cx, "files.entry.1");
    let mut previous: Option<gpui::Bounds<Pixels>> = None;
    for selector in [
        "files.download.1",
        "files.to_local.1",
        "files.rename.1",
        "files.delete.1",
    ] {
        assert_pointer_target(cx, selector);
        let bounds = bounds_of(cx, selector);
        assert!(
            bounds.origin.x >= entry.origin.x + entry.size.width - px(0.5),
            "{selector} starts over the entry column: {bounds:?} vs {entry:?}"
        );
        assert!(
            bounds.origin.x + bounds.size.width <= row.origin.x + row.size.width + px(0.5),
            "{selector} leaves the row it belongs to: {bounds:?} in {row:?}"
        );
        if let Some(previous) = previous {
            assert!(
                bounds.origin.x >= previous.origin.x + previous.size.width - px(0.5),
                "{selector} overlaps the button before it: {bounds:?} after {previous:?}"
            );
        }
        previous = Some(bounds);
    }
}

#[gpui::test]
async fn the_session_bar_and_file_row_controls_are_pointer_targets_too(cx: &mut TestAppContext) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    smallest_text_size(&view, cx);
    // The session bar: closing the session, splitting the pane it shows, and the tab
    // that holds it.
    for selector in [
        "session.close",
        "session.tab.close.0",
        "pane.split.right",
        "pane.split.down",
    ] {
        assert_pointer_target(cx, selector);
    }

    // The file list: downloading, sending back and the two row actions, in a column
    // that is as wide as the four of them need.
    let _answers = open_files(cx, &view, id);
    assert_file_actions_keep_their_column(cx);

    // A toast's close button is a target too; copying a path puts one on screen.
    right_click(cx, "files.row.1");
    click(cx, "files.menu.copy_path");
    assert_pointer_target(cx, "toast.close.0");

    // The largest text doubles the buttons, so the column has to follow them there
    // too: this is where a fixed width would have left the entry covered.
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest,
        "the file row is measured at 200% text"
    );
    assert_file_actions_keep_their_column(cx);

    // And at the smallest window with that text, where the row has the least room to
    // give: the four targets still fit inside it.
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    assert_file_actions_keep_their_column(cx);
}

#[gpui::test]
async fn the_split_nudge_buttons_are_targets_larger_than_the_band_they_sit_in(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    live_remote(cx, &view);
    smallest_text_size(&view, cx);
    click(cx, "pane.split.right");
    assert_nudges_of_one_split(cx, &view);
}

#[gpui::test]
async fn the_stacked_split_nudges_are_targets_too(cx: &mut TestAppContext) {
    // The same two buttons on a divider that runs the other way: here they take the
    // band's height instead of its width.
    let (view, cx) = remote_view(cx);
    live_remote(cx, &view);
    smallest_text_size(&view, cx);
    click(cx, "pane.split.down");
    assert_nudges_of_one_split(cx, &view);
}

/// Checks the divider of the one split on screen, whichever way it runs.
///
/// The band keeps its 12 thickness; the nudge buttons it carries are pointer targets in
/// their own right and are drawn over the neighbouring pane's edge instead of being
/// squeezed to the band (R4).
fn assert_nudges_of_one_split(cx: &mut VisualTestContext, view: &Entity<ShelluneView>) {
    let band = bounds_of(cx, "pane.divider");
    let across = band.size.height > band.size.width;
    let normal = if across {
        band.size.width
    } else {
        band.size.height
    };
    assert!(
        normal < px(24.0),
        "the divider is still a thin drag strip: {band:?}"
    );
    let less = bounds_of(cx, "pane.resize.less");
    let more = bounds_of(cx, "pane.resize.more");
    for (selector, bounds) in [("pane.resize.less", less), ("pane.resize.more", more)] {
        assert!(
            bounds.size.width >= px(24.0) && bounds.size.height >= px(24.0),
            "{selector} is a pointer target smaller than 24x24: {bounds:?}"
        );
        let along_normal = if across {
            bounds.size.width
        } else {
            bounds.size.height
        };
        assert!(
            along_normal > normal,
            "{selector} was squeezed into the {band:?} band instead of keeping its own size"
        );
        // It overhangs the band the same amount on both sides, so it covers neither
        // neighbour more than the other.
        let under = if across {
            bounds.center().x - band.center().x
        } else {
            bounds.center().y - band.center().y
        };
        assert!(
            under.abs() <= px(1.0),
            "{selector} overhangs the band on one side only: {bounds:?} in {band:?}"
        );
    }
    // The buttons sit in the middle of the band, side by side, so their hit areas do
    // not overlap and each stays inside the window.
    let (less_end, more_start) = if across {
        (less.origin.y + less.size.height, more.origin.y)
    } else {
        (less.origin.x + less.size.width, more.origin.x)
    };
    assert!(
        more_start >= less_end - px(0.5),
        "the two nudge targets overlap: {less:?} and {more:?}"
    );
    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_fits_inside(less, viewport, "pane.resize.less");
    assert_fits_inside(more, viewport, "pane.resize.more");

    // Both buttons still nudge the split.
    let start = cx.update(|_window, app| view.read(app).layout_ratios_for_test());
    click(cx, "pane.resize.more");
    let grown = cx.update(|_window, app| view.read(app).layout_ratios_for_test());
    assert!(
        grown[0].1 > start[0].1,
        "the nudge button moved the divider: {start:?} then {grown:?}"
    );
    click(cx, "pane.resize.less");
    let back = cx.update(|_window, app| view.read(app).layout_ratios_for_test());
    assert_eq!(
        back[0], start[0],
        "the other button nudges the other way: {grown:?} then {back:?}"
    );

    // The band left outside the buttons is still a drag strip: the buttons cover the
    // middle of the band, not all of it.
    let grab = if across {
        point(more.center().x, less.origin.y - px(10.0))
    } else {
        point(less.origin.x - px(10.0), more.center().y)
    };
    cx.simulate_mouse_move(grab, None, Modifiers::default());
    cx.simulate_mouse_down(grab, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let released = if across {
        point(grab.x + px(80.0), grab.y)
    } else {
        point(grab.x, grab.y + px(80.0))
    };
    cx.simulate_mouse_move(released, Some(MouseButton::Left), Modifiers::default());
    cx.run_until_parked();
    cx.simulate_mouse_up(released, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    let dragged = cx.update(|_window, app| view.read(app).layout_ratios_for_test());
    assert!(
        dragged[0].1 > back[0].1,
        "a press on the band outside the buttons still drags the divider: {back:?} then {dragged:?}"
    );
}

#[test]
fn the_terminal_toolbar_and_menu_read_the_bindings_in_force() {
    let mut keymap = Keymap::defaults(Platform::current());
    let defaults = TerminalShortcuts::of(&keymap);
    assert_eq!(
        defaults.search.as_deref(),
        Some(
            crate::app::keymap::keystroke_label(
                Platform::current().search_keystroke(),
                Platform::current()
            )
            .as_str()
        ),
        "the search hint names the platform's own key"
    );
    assert!(defaults.copy.is_some() && defaults.paste.is_some());

    keymap.unbind("terminal.copy");
    keymap
        .set("terminal.paste", "ctrl-alt-v")
        .expect("a free combination");
    let changed = TerminalShortcuts::of(&keymap);
    assert_eq!(changed.copy, None, "an unbound action names no key");
    assert_eq!(
        changed.paste,
        Some(crate::app::keymap::keystroke_label(
            "ctrl-alt-v",
            Platform::current()
        ))
    );
}

#[gpui::test]
async fn the_terminal_menu_names_the_shortcuts_in_force(cx: &mut TestAppContext) {
    let (_pane, cx) = pane_with("", cx);
    right_click(cx, "terminal.viewport");
    assert!(
        cx.debug_bounds("terminal.menu").is_some(),
        "the right-click menu opens on the grid"
    );
    for selector in [
        "terminal.menu.copy.shortcut",
        "terminal.menu.paste.shortcut",
        "terminal.menu.search.shortcut",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "{selector} names the binding in force instead of a literal"
        );
    }
}

#[gpui::test]
async fn an_unbound_shortcut_is_not_named_in_the_terminal_menu(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    let mut keymap = Keymap::defaults(Platform::current());
    keymap.unbind("terminal.copy");
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.set_shortcuts(TerminalShortcuts::of(&keymap), cx);
        });
    });
    cx.run_until_parked();
    right_click(cx, "terminal.viewport");
    assert!(
        cx.debug_bounds("terminal.menu.copy.shortcut").is_none(),
        "a key the window no longer answers is not offered in the menu"
    );
    assert!(
        cx.debug_bounds("terminal.menu.paste.shortcut").is_some(),
        "the actions still bound keep their hint"
    );
}

#[gpui::test]
async fn a_shortcut_can_be_changed_with_the_keyboard_alone(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    // Going to the settings with the shortcut leaves the keyboard on the page.
    let settings = Keymap::defaults(Platform::current())
        .keystroke("nav.settings")
        .expect("the settings shortcut")
        .to_owned();
    cx.simulate_keystrokes(&settings);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Settings);

    click(cx, "settings.category.keys");

    // The arrow keys pick a row and Enter opens it: no pointer is used here.
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let capturing = cx.update(|_window, app| view.read(app).capturing_for());
    assert!(
        capturing.is_some(),
        "Enter opens the row the keyboard is on"
    );
    cx.simulate_keystrokes("ctrl-alt-j");
    cx.run_until_parked();
    let capturing = capturing.expect("the row that was captured");
    assert_eq!(
        cx.update(|_window, app| view.read(app).capturing_for()),
        None
    );
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .keymap()
            .keystroke(capturing)
            .map(str::to_owned)),
        Some("ctrl-alt-j".to_owned()),
        "the keyboard alone changed the binding"
    );
}

#[gpui::test]
async fn a_shortcut_can_be_unbound_and_restored_with_the_keyboard_alone(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    let settings = Keymap::defaults(Platform::current())
        .keystroke("nav.settings")
        .expect("the settings shortcut")
        .to_owned();
    cx.simulate_keystrokes(&settings);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Settings);

    // The shortcut page holds the list; the keyboard is the only input it needs.
    click(cx, "settings.category.keys");
    type_into(cx, "settings.keys.search", "copy");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view
            .read(app)
            .keymap()
            .matching("copy", copy::Locale::English)
            .len()),
        1,
        "the search leaves the copy action as the only row"
    );

    // ↓ takes the keyboard into the list and onto the row's first control.
    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).key_row_control(app)),
        Some(KeyRowControl::Change)
    );

    // → steps to Unbind, and Enter hands the keys back to the program.
    cx.simulate_keystrokes("right");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).key_row_control(app)),
        Some(KeyRowControl::Unbind)
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).keymap().is_unbound("terminal.copy")),
        "Enter on Unbind leaves the action without a shortcut"
    );

    // An unbound row still offers Restore, which Enter runs: the cursor is on the
    // row's last control, so the same key puts the default back.
    assert!(
        cx.debug_bounds("settings.keys.reset.terminal.copy")
            .is_some(),
        "an unbound action can be restored without reaching for the pointer"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).key_row_control(app)),
        Some(KeyRowControl::Reset)
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(
        cx.update(|_window, app| view.read(app).keymap().is_default("terminal.copy")),
        "Enter on Restore puts the default binding back"
    );
}

#[gpui::test]
async fn two_hundred_percent_chinese_settings_shortcuts_stay_inside_the_window(
    cx: &mut TestAppContext,
) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    // The minimum width, with the room the rows need to be laid out: the point is
    // that nothing is wider than the window, not how far the page scrolls.
    cx.simulate_resize(size(px(MIN_WINDOW_SIZE.0), px(2400.0)));
    focus_root(&view, cx);
    cx.dispatch_action(ToggleLanguage);
    cx.dispatch_action(CycleTextSize);
    cx.dispatch_action(CycleTextSize);
    cx.run_until_parked();
    cx.dispatch_action(crate::app::actions::GoToSettings);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Settings);
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest
    );
    click(cx, "settings.category.keys");
    let viewport = cx.update(|window, _app| window.viewport_size());
    for selector in [
        "settings.keys",
        "settings.keys.row.palette.open",
        "settings.keys.reset_all",
    ] {
        assert_within_width(bounds_of(cx, selector), viewport, selector);
    }
}

#[test]
fn a_rebound_action_survives_a_restart_through_the_preferences_file() {
    let dir = std::env::temp_dir().join(format!("shellune-keymap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let store = shellune_storage::PreferencesStore::at(&dir);
    let mut keymap = Keymap::defaults(Platform::current());
    keymap
        .set("terminal.search", "ctrl-alt-y")
        .expect("a free combination");
    keymap.unbind("terminal.paste");
    let saved = keymap.to_preferences(None);
    store
        .save(&shellune_storage::Preferences {
            keymap: Some(saved),
            ..Default::default()
        })
        .expect("the preferences are written");

    // What a restart does: read the file and resolve the bindings.
    let (read, problem) = store.load_reported();
    assert_eq!(problem, None);
    let (restored, keymap_problem) = Keymap::resolve(Platform::current(), read.keymap.as_ref());
    assert_eq!(keymap_problem, None);
    assert_eq!(restored.keystroke("terminal.search"), Some("ctrl-alt-y"));
    assert!(restored.is_unbound("terminal.paste"));
    assert_eq!(
        restored.keystroke("terminal.copy"),
        Keymap::defaults(Platform::current()).keystroke("terminal.copy"),
        "the actions the user did not touch keep their defaults"
    );
    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// Background notices (R7)
// ---------------------------------------------------------------------------

/// Sends bytes to one session's terminal as if its program had written them.
fn feed_session(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
    bytes: &[u8],
) {
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(id).cloned())
        .expect("the session has a terminal");
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.feed_for_test(bytes, cx));
    });
    cx.run_until_parked();
}

/// The background marks one session's tab is carrying.
fn unread_of(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    id: SessionId,
) -> crate::app::workspace::UnreadNotices {
    cx.update(|_window, app| view.read(app).workspace().session_unread(id))
}

/// A command the shell reports from start to end, as its integration markers do.
const REPORTED_COMMAND: &[u8] =
    b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07secret output\r\n\x1b]133;D;0\x07";

#[gpui::test]
async fn a_bell_in_a_background_tab_marks_it_without_taking_the_screen(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let (background, foreground) = (tabs[0], tabs[1]);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(foreground),
        "the newest shell is the one on screen"
    );

    feed_session(cx, &view, background, b"\x07");

    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(foreground),
        "a bell in another tab must not switch the tab or move the keyboard"
    );
    assert!(
        unread_of(cx, &view, background).bell,
        "the tab the bell rang in is marked"
    );
    assert!(
        !unread_of(cx, &view, background).finished,
        "a ring is not a command end"
    );
    assert!(
        unread_of(cx, &view, foreground).is_empty(),
        "the tab on screen is not marked for another tab's bell"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.bell.0").is_some(),
        "the strip marks that tab in words, not only in colour"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.bell.1").is_none(),
        "the tab on screen carries no mark"
    );
    assert!(
        cx.update(|_window, app| view.read(app).toast_titles())
            .is_empty(),
        "a background bell is not a toast that disappears on its own"
    );

    // Choosing the mark is choosing that tab: it shows the tab and the pane that rang,
    // which is what clears the mark.
    click(cx, "session.tab.mark.bell.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(background)
    );
    assert!(
        unread_of(cx, &view, background).is_empty(),
        "viewing the tab cleared its mark"
    );
    // The strip draws from exactly this state, so a cleared tab has nothing to draw.
    // (The test window keeps a selector's last bounds, so absence is read from the
    // model the strip renders rather than from a selector that once existed.)
    assert!(cx.debug_bounds("session.tab.mark.bell.1").is_none());
}

#[gpui::test]
async fn only_a_shell_reported_command_end_marks_a_background_tab(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let background = tabs[0];

    // Output on its own says nothing about a command ending.
    feed_session(cx, &view, background, b"just output\r\n");
    assert!(
        unread_of(cx, &view, background).is_empty(),
        "output alone must not claim a command ended"
    );

    // A prompt that arrived without an end marker is not one either: this is the
    // shell-without-integration case, and it must not produce a made-up reminder.
    feed_session(
        cx,
        &view,
        background,
        b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;A\x07",
    );
    assert!(
        unread_of(cx, &view, background).is_empty(),
        "without an end marker the shell never reported one"
    );

    // The shell reporting the end marks the tab, and the mark is one fixed word.
    feed_session(cx, &view, background, REPORTED_COMMAND);
    let unread = unread_of(cx, &view, background);
    assert!(
        unread.finished,
        "the reported end marks the tab it happened in"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(tabs[1]),
        "the mark does not switch the tab"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.finished.0").is_some(),
        "the strip says a command ended"
    );
    let label = cx.update(|_window, app| view.read(app).tab_label_for_test(background));
    assert!(
        !label.contains("secret output"),
        "the mark repeats no output, got {label:?}"
    );
    let strings = &ENGLISH;
    assert_eq!(
        strings.tab_mark_finished, "Ended",
        "the mark is a fixed word, so it cannot carry what ran"
    );

    // The mark clears when the tab is seen again, and the strip draws only that state.
    click(cx, "session.tab.0");
    assert!(
        unread_of(cx, &view, background).is_empty(),
        "viewing the tab cleared the command-end mark"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.finished.1").is_none(),
        "the other tab never carried one"
    );
}

#[gpui::test]
async fn the_tab_list_shows_the_marks_a_tab_carries(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let background = cx.update(|_window, app| view.read(app).open_tabs())[0];
    feed_session(cx, &view, background, b"\x07");
    feed_session(cx, &view, background, REPORTED_COMMAND);

    click(cx, "tabs.list.open");
    assert!(
        cx.debug_bounds("tabs.list.mark.bell.0").is_some(),
        "the list spells out the bell it is carrying"
    );
    assert!(
        cx.debug_bounds("tabs.list.mark.finished.0").is_some(),
        "and the command end"
    );
    // Choosing the row shows that tab, which is also what clears its marks.
    click(cx, "tabs.list.row.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(background)
    );
    assert!(unread_of(cx, &view, background).is_empty());
}

#[gpui::test]
async fn the_two_reminders_switch_separately_and_are_written_down(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let background = tabs[0];
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    click(cx, "settings.category.notices");
    click(cx, "settings.item.bell-notice");
    assert!(
        cx.debug_bounds("settings.notice.bell").is_some(),
        "the settings page offers the bell reminder"
    );
    click(cx, "settings.item.finished-notice");
    assert!(
        cx.debug_bounds("settings.notice.finished").is_some(),
        "and the command-end reminder separately"
    );

    // Both start on; switching the bell off leaves the command one alone.
    click(cx, "settings.item.bell-notice");
    click(cx, "settings.notice.bell.1");
    let settings = cx.update(|_window, app| view.read(app).workspace().attention_settings());
    assert!(!settings.bell, "the bell reminder is off");
    assert!(settings.finished, "the command reminder is untouched");

    // The choice is what the window would write, so a restart can read it back.
    let written = cx.update(|_window, app| view.read(app).current_preferences_for_test());
    assert_eq!(written.bell_notice, Some(false));
    assert_eq!(written.command_notice, Some(true));

    // Back to the terminal: a bell is no longer marked, a reported end still is.
    click(cx, "session.tab.1");
    feed_session(cx, &view, background, b"\x07");
    assert!(
        unread_of(cx, &view, background).is_empty(),
        "a switched-off reminder marks nothing"
    );
    feed_session(cx, &view, background, REPORTED_COMMAND);
    assert!(
        unread_of(cx, &view, background).finished,
        "the other reminder still works on its own"
    );
    // Switching a reminder off leaves the blocks the shell reported as they are.
    let blocks = cx.update(|_window, app| {
        let pane = view.read(app).pane_of(background).cloned();
        pane.map(|pane| pane.read(app).command_block_count_for_test())
    });
    assert_eq!(
        blocks,
        Some(1),
        "the command-end reminder is a mark, not the record of the block"
    );

    // And switching it off removes the mark it already made.
    click(cx, "nav.settings");
    click(cx, "settings.item.finished-notice");
    click(cx, "settings.notice.finished.1");
    assert!(
        unread_of(cx, &view, background).is_empty(),
        "no mark of a kind whose reminder is off is left behind"
    );
}

#[test]
fn the_reminders_survive_a_restart_through_the_preferences_file() {
    let dir = std::env::temp_dir().join(format!("shellune-notices-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let store = shellune_storage::PreferencesStore::at(&dir);
    store
        .save(&shellune_storage::Preferences {
            bell_notice: Some(false),
            command_notice: Some(true),
            ..Default::default()
        })
        .expect("the preferences are written");

    // What a restart does: read the file, then apply it to a new window.
    let (read, problem) = store.load_reported();
    assert_eq!(problem, None);
    assert_eq!(read.bell_notice, Some(false));
    assert_eq!(read.command_notice, Some(true));

    // A file written before these fields existed keeps both reminders on.
    let older: shellune_storage::Preferences =
        serde_json::from_str(r#"{"theme":"dark"}"#).expect("an older file still reads");
    assert_eq!(older.bell_notice, None);
    assert_eq!(older.command_notice, None);
    let _ = std::fs::remove_dir_all(dir);
}

#[gpui::test]
async fn a_saved_choice_of_reminders_is_applied_on_the_next_start(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.run_until_parked();
    let saved = shellune_storage::Preferences {
        bell_notice: Some(false),
        command_notice: Some(true),
        ..Default::default()
    };
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.use_saved_preferences_for_test(&saved, cx);
        });
    });
    cx.run_until_parked();
    let settings = cx.update(|_window, app| view.read(app).workspace().attention_settings());
    assert!(
        !settings.bell,
        "the saved choice is what the window starts with"
    );
    assert!(settings.finished);
}

#[test]
fn the_workspace_marks_and_clears_one_tab_and_respects_each_switch() {
    let shells = shellune_terminal::discover();
    let Some(shell) = shells.first() else {
        return;
    };
    let mut workspace = Workspace::new();
    let first = workspace.register_local_session(shell);
    let second = workspace.register_local_session(shell);

    // Both reminders start on, and a mark belongs to the tab the news came from.
    assert_eq!(workspace.attention_settings(), AttentionSettings::default());
    assert!(workspace.attention_settings().bell && workspace.attention_settings().finished);
    assert!(workspace.note_attention(first.id(), AttentionKind::Bell));
    assert!(workspace.session_unread(first.id()).bell);
    assert!(
        workspace.session_unread(second.id()).is_empty(),
        "another session's tab is not marked"
    );

    // A switched-off kind marks nothing, and drops what it had already marked.
    assert!(workspace.set_attention(AttentionKind::Bell, false));
    assert!(
        !workspace.session_unread(first.id()).bell,
        "the mark that was already there is dropped with the reminder"
    );
    assert!(workspace.session_unread(first.id()).is_empty());
    assert!(!workspace.note_attention(first.id(), AttentionKind::Bell));
    assert!(workspace.note_attention(first.id(), AttentionKind::CommandFinished));

    // Looking at the tab clears it.
    assert!(workspace.clear_session_attention(first.id()));
    assert!(workspace.session_unread(first.id()).is_empty());

    // Closing the session takes the mark's tab with it.
    assert!(
        workspace.set_attention(AttentionKind::Bell, true),
        "a reminder can be switched back on"
    );
    assert!(
        !workspace.set_attention(AttentionKind::Bell, true),
        "switching it to what it already is changes nothing"
    );
    assert!(workspace.note_attention(second.id(), AttentionKind::Bell));
    assert!(workspace.session_closed(second.id()));
    assert!(workspace.session_unread(second.id()).is_empty());
}

#[gpui::test]
async fn a_pane_holds_the_display_status_until_the_window_takes_it(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    let tag = cx.update(|_window, app| pane.read(app).tag());
    let ready = X11Status::Ready {
        display: ":96".to_owned(),
    };
    assert!(
        cx.update(
            |_window, app| pane.update(app, |pane, cx| pane.adopt_events(
                tag,
                vec![SourceEvent::X11(ready.clone())],
                cx
            ))
        ),
        "the status belongs to this pane"
    );
    assert_eq!(
        cx.update(|_window, app| pane.update(app, |pane, _cx| pane.take_x11_status())),
        Some(ready),
        "the window reads what the session reported"
    );
    assert_eq!(
        cx.update(|_window, app| pane.update(app, |pane, _cx| pane.take_x11_status())),
        None,
        "the status is news once, not a value on every frame"
    );
    // A display that failed is a status, not the end of the terminal.
    let pane_state = cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.adopt_events(
                tag,
                vec![SourceEvent::X11(X11Status::Failed {
                    reason: "no local X display is available".to_owned(),
                    next_step: "Start an X server.".to_owned(),
                })],
                cx,
            );
            pane.state().clone()
        })
    });
    assert_eq!(pane_state, PaneState::Live);
}

#[gpui::test]
async fn a_pane_keeps_a_background_notice_until_it_is_collected(cx: &mut TestAppContext) {
    let (pane, cx) = pane_with("", cx);
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.feed_for_test(b"\x07", cx));
    });
    let pending = cx.update(|_window, app| pane.read(app).pending_attention_for_test());
    assert_eq!(
        pending,
        TerminalAttention {
            bell: true,
            finished: false
        }
    );

    // The window collects it, and the same bell is never marked twice.
    let taken = cx.update(|_window, app| pane.update(app, |pane, _cx| pane.take_attention()));
    assert_eq!(taken, pending);
    assert!(
        cx.update(|_window, app| pane.read(app).pending_attention_for_test())
            .is_empty(),
        "the notice is taken once, not repeated"
    );
    // Plain output still says nothing about a command ending.
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.feed_for_test(b"plain output\r\n", cx));
    });
    assert!(
        cx.update(|_window, app| pane.read(app).pending_attention_for_test())
            .is_empty()
    );
}

#[gpui::test]
async fn a_background_session_that_ends_keeps_its_reason_and_the_others_work(
    cx: &mut TestAppContext,
) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    let (ended, kept) = (tabs[0], tabs[1]);
    let pane = cx
        .update(|_window, app| view.read(app).pane_of(ended).cloned())
        .expect("the background session has a terminal");

    // Leave that shell the way a user does, while the other tab is on screen.
    type_into_pane(cx, &pane, "exit\n");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut exited = false;
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| {
                pane.pump_once(cx);
            });
        });
        exited =
            cx.update(|_window, app| matches!(pane.read(app).state(), PaneState::Exited { .. }));
        if exited {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(exited, "the shell exit was never noticed by the pane");
    cx.run_until_parked();

    // The tab that ended states what happened, and the session on screen is untouched.
    let state = session_state(cx, &view, ended);
    let reason = match &state {
        Some(SessionState::Disconnected(failure)) | Some(SessionState::Failed(failure)) => {
            Some(failure.to_string())
        }
        _ => None,
    };
    assert!(
        reason.is_some(),
        "a session that ended keeps a reason, got {state:?}"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(kept),
        "a background session ending does not move the user to it"
    );
    assert_eq!(
        session_state(cx, &view, kept),
        Some(SessionState::Connected),
        "the other session keeps working"
    );
    // A disconnect is a state with a reason, not a toast that disappears on its own.
    assert!(
        cx.update(|_window, app| view.read(app).toast_titles())
            .is_empty(),
        "the reason is not left to a toast"
    );
    // While the ended tab is in the background, its banner is not what the window shows.
    assert!(
        cx.debug_bounds("terminal.failure").is_none(),
        "another tab's terminal is still the one on screen"
    );

    // Choosing the tab shows the reason and the next step, and they stay there.
    click(cx, "session.tab.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(ended)
    );
    assert!(
        cx.debug_bounds("terminal.failure").is_some(),
        "the reason the session ended is visible on its own tab"
    );
    assert!(
        cx.debug_bounds("terminal.failure.next").is_some(),
        "and the step that can follow it"
    );
    assert_eq!(
        session_state(cx, &view, ended),
        state,
        "looking at the tab does not clear the reason"
    );
}

#[gpui::test]
async fn a_mark_is_still_drawn_when_motion_is_reduced(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let background = cx.update(|_window, app| view.read(app).open_tabs())[0];
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "nav.settings");
    click(cx, "settings.item.motion");
    click(cx, "settings.motion.1");
    assert!(
        !cx.update(|_window, app| view.read(app).workspace().motion())
            .allows_animation(),
        "reduced motion is on"
    );

    click(cx, "session.tab.1");
    feed_session(cx, &view, background, b"\x07");
    // The mark is a static word: there is no motion to suppress, so it is drawn whole
    // the moment the news arrives, and it never blinks (R7).
    assert!(
        cx.debug_bounds("session.tab.mark.bell.0").is_some(),
        "a reduced-motion window still says what happened"
    );
    assert!(unread_of(cx, &view, background).bell);
}

/// One shell, split sideways, so one tab holds two panes at once (R4, R7).
///
/// Returns the sessions of the two panes in tree order and the one that receives input,
/// or `None` when the machine has no shell to start.
fn split_tab(
    cx: &mut TestAppContext,
) -> Option<(
    Entity<ShelluneView>,
    &mut VisualTestContext,
    SessionId,
    SessionId,
)> {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(design_window());
    cx.run_until_parked();
    if cx.update(|_window, app| view.read(app).workspace().shells().is_empty()) {
        return None;
    }
    click(cx, "header.primary");
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.split_active_for_test(SplitAxis::LeftRight, window, cx);
        });
    });
    cx.run_until_parked();
    let sessions: Vec<SessionId> = cx
        .update(|_window, app| view.read(app).active_leaf_sessions_for_test())
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(sessions.len(), 2, "the tab holds two started panes");
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs().len()),
        1,
        "the split is still one tab"
    );
    let (left, right) = (sessions[0], sessions[1]);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(right),
        "the newest pane is the one that receives input"
    );
    Some((view, cx, left, right))
}

#[gpui::test]
async fn a_bell_or_a_reported_end_in_a_split_pane_on_screen_marks_nothing(cx: &mut TestAppContext) {
    let Some((view, cx, unfocused, focused)) = split_tab(cx) else {
        return;
    };

    // The unfocused pane is on screen beside the one being typed into, so its bell is
    // news the user is already looking at: it marks nothing, on this tab or any other.
    feed_session(cx, &view, unfocused, b"\x07");
    assert!(
        unread_of(cx, &view, unfocused).is_empty(),
        "a bell in the visible pane of the tab on screen is not a background notice"
    );
    assert!(unread_of(cx, &view, focused).is_empty());
    assert!(
        cx.debug_bounds("session.tab.mark.bell.0").is_none(),
        "the tab on screen carries no bell mark"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(focused),
        "the bell moved no focus"
    );

    // The same holds for a command end the shell itself reported there.
    feed_session(cx, &view, unfocused, REPORTED_COMMAND);
    assert!(
        unread_of(cx, &view, unfocused).is_empty(),
        "a reported end in the visible pane marks nothing either"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.finished.0").is_none(),
        "the tab on screen carries no command-end mark"
    );
    // The block is still recorded: the reminder is what is absent, not the record.
    let blocks = cx.update(|_window, app| {
        view.read(app)
            .pane_of(unfocused)
            .map(|pane| pane.read(app).command_block_count_for_test())
    });
    assert_eq!(blocks, Some(1), "the shell's command block is still kept");

    // A bell in the pane that has the keyboard is not a reminder at all.
    feed_session(cx, &view, focused, b"\x07");
    feed_session(cx, &view, focused, REPORTED_COMMAND);
    assert!(
        unread_of(cx, &view, focused).is_empty(),
        "the pane being typed into never marks its own tab"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.bell.0").is_none()
            && cx.debug_bounds("session.tab.mark.finished.0").is_none(),
        "a split tab on screen stays unmarked"
    );
}

#[gpui::test]
async fn a_marked_split_tab_is_cleared_when_it_is_looked_at(cx: &mut TestAppContext) {
    let Some((view, cx, unfocused, focused)) = split_tab(cx) else {
        return;
    };
    // A second tab takes the screen, so the first one is now in the background.
    click(cx, "header.primary");
    let other = cx
        .update(|_window, app| view.read(app).active_session())
        .filter(|id| *id != unfocused && *id != focused)
        .expect("the new shell is the tab on screen");

    // Both panes of the background tab are in that background tab, so each kind of news
    // from either of them marks that tab and only that tab (R7).
    feed_session(cx, &view, unfocused, b"\x07");
    feed_session(cx, &view, focused, REPORTED_COMMAND);
    assert!(
        unread_of(cx, &view, unfocused).bell,
        "the bell marks the tab it happened in"
    );
    assert!(
        unread_of(cx, &view, focused).finished,
        "and the reported end marks the same tab, whichever pane it came from"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(other),
        "a background mark takes no focus and switches no tab"
    );
    assert!(
        unread_of(cx, &view, other).is_empty(),
        "the tab on screen is not marked for another tab's news"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.bell.0").is_some()
            && cx.debug_bounds("session.tab.mark.finished.0").is_some(),
        "the strip says what the background tab is carrying"
    );
    assert!(
        cx.debug_bounds("session.tab.mark.bell.1").is_none(),
        "the tab on screen stays unmarked"
    );

    // Looking at the marked tab clears both marks, and the pane with the keyboard in it
    // is the one the layout calls active, whether or not it was the pane that rang.
    click(cx, "session.tab.0");
    assert!(
        unread_of(cx, &view, unfocused).is_empty() && unread_of(cx, &view, focused).is_empty(),
        "viewing the tab cleared what it was carrying"
    );

    // Moving focus to the other pane of the same tab keeps it clear: the tab is being
    // looked at as a whole, and switching panes inside it is not new news.
    cx.update(|window, app| {
        view.update(app, |view, cx| {
            view.focus_direction_for_test(Direction::Left, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(unfocused),
        "the keyboard moved to the other pane"
    );
    assert!(
        unread_of(cx, &view, unfocused).is_empty() && unread_of(cx, &view, focused).is_empty(),
        "a focus change inside the tab leaves no mark behind"
    );
    feed_session(cx, &view, focused, b"\x07");
    feed_session(cx, &view, focused, REPORTED_COMMAND);
    assert!(
        unread_of(cx, &view, focused).is_empty(),
        "the panes beside the focused one are seen, so they still mark nothing"
    );
}

/// Asserts a measured box stays inside another box, on both axes.
///
/// A tab draws its marks and its close button inside its own box; anything measured
/// outside it would be drawn over the neighbouring tab, and a pointer landing there
/// would act on that neighbour instead (R4, R7).
fn assert_stays_inside_box(inner: gpui::Bounds<Pixels>, outer: gpui::Bounds<Pixels>, what: &str) {
    assert!(
        inner.size.width > px(0.0) && inner.size.height > px(0.0),
        "{what} was measured with an empty box: {inner:?}"
    );
    assert!(
        inner.origin.x >= outer.origin.x - px(0.5)
            && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + px(0.5),
        "{what} leaves its box sideways: {inner:?} in {outer:?}"
    );
    assert!(
        inner.origin.y >= outer.origin.y - px(0.5)
            && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + px(0.5),
        "{what} leaves its box vertically: {inner:?} in {outer:?}"
    );
}

#[gpui::test]
async fn a_marked_tab_keeps_its_marks_and_close_button_inside_itself(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    // Enough tabs that the strip cannot give any of them more than its minimum: the
    // state in which a tab's own content used to be squeezed outward.
    for _ in 0..6 {
        click(cx, "header.primary");
    }
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    assert_eq!(
        tabs.len(),
        8,
        "the strip is crowded enough to shrink every tab"
    );
    let marked = tabs[1];
    feed_session(cx, &view, marked, b"\x07");
    feed_session(cx, &view, marked, REPORTED_COMMAND);
    assert!(
        unread_of(cx, &view, marked).bell && unread_of(cx, &view, marked).finished,
        "the background tab carries both marks"
    );

    // The marked tab is wider than a bare one, because its two words need the room;
    // they are not squeezed into a title's width.
    let tab = bounds_of(cx, "session.tab.1");
    let bare = bounds_of(cx, "session.tab.2");
    assert!(
        tab.size.width > bare.size.width,
        "the marked tab reserves room for its words: {tab:?} against {bare:?}"
    );

    // Every part of the tab stays inside the tab, so nothing is drawn over the next
    // tab, and the close button is the one the pointer finds inside this tab.
    for selector in [
        "session.tab.mark.bell.1",
        "session.tab.mark.finished.1",
        "session.tab.close.1",
    ] {
        assert_stays_inside_box(bounds_of(cx, selector), tab, selector);
    }
    assert_pointer_target(cx, "session.tab.close.1");
    assert!(
        bare.origin.x >= tab.origin.x + tab.size.width - px(0.5),
        "the next tab starts after the marked one: {tab:?} then {bare:?}"
    );

    // Choosing the close button of the marked tab closes that tab and only it.
    click(cx, "session.tab.close.1");
    let mut expected = tabs;
    expected.remove(1);
    assert_eq!(
        cx.update(|_window, app| view.read(app).open_tabs()),
        expected,
        "the close button belongs to the tab it is drawn in, not to its neighbour"
    );
}

#[gpui::test]
async fn the_tab_list_keeps_the_marks_readable_in_the_smallest_window_at_large_text(
    cx: &mut TestAppContext,
) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let background = cx.update(|_window, app| view.read(app).open_tabs())[0];
    let viewport = minimum_window();
    cx.simulate_resize(viewport);
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    feed_session(cx, &view, background, b"\x07");
    feed_session(cx, &view, background, REPORTED_COMMAND);

    // The strip has no room for a tab at this size, so its marks are clipped with the
    // tab they belong to (R4 allows the strip to fall back); inside the tab's own box
    // they still stay put rather than sliding over the neighbouring tab.
    let tab = bounds_of(cx, "session.tab.0");
    for selector in [
        "session.tab.mark.bell.0",
        "session.tab.mark.finished.0",
        "session.tab.close.0",
    ] {
        assert_stays_inside_box(bounds_of(cx, selector), tab, selector);
    }
    assert_pointer_target(cx, "session.tab.close.0");

    // The same holds in Chinese, where the two words are two full-width characters.
    cx.dispatch_action(ToggleLanguage);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese,
        "the marks are measured in Chinese too"
    );
    let tab = bounds_of(cx, "session.tab.0");
    for selector in [
        "session.tab.mark.bell.0",
        "session.tab.mark.finished.0",
        "session.tab.close.0",
    ] {
        assert_stays_inside_box(bounds_of(cx, selector), tab, selector);
    }

    // The list of every tab is the way to read the marks here: it fits the window and
    // writes each mark out in full.
    assert_fits_inside(
        bounds_of(cx, "tabs.list.open"),
        viewport,
        "the all-tabs control",
    );
    assert_pointer_target(cx, "tabs.list.open");
    click(cx, "tabs.list.open");
    assert_fits_inside(bounds_of(cx, "tabs.list.dialog"), viewport, "the tab list");
    let row = bounds_of(cx, "tabs.list.row.0");
    assert_fits_inside(row, viewport, "the row of the marked tab");
    for selector in ["tabs.list.mark.bell.0", "tabs.list.mark.finished.0"] {
        assert_stays_inside_box(bounds_of(cx, selector), row, selector);
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    assert_pointer_target(cx, "tabs.list.rename.0");

    // Choosing the row shows that tab and clears what it was carrying (R7).
    click(cx, "tabs.list.row.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(background)
    );
    assert!(
        unread_of(cx, &view, background).is_empty(),
        "looking at the tab cleared the marks the list showed"
    );
}

// ---------------------------------------------------------------------------
// Saved shortcut commands (R6)
// ---------------------------------------------------------------------------

/// Mounts the shell on a directory of its own, so a test's saved commands are its own too.
fn snippet_view<'a>(
    cx: &'a mut TestAppContext,
    dir: &TempDir,
) -> (Entity<ShelluneView>, &'a mut VisualTestContext) {
    let workspace = Workspace::new();
    let store = dir.store();
    let trust = TrustStore::at(dir.path());
    let (view, cx) = cx.add_window_view(move |_window, cx| {
        ShelluneView::with_store_and_trust(cx, workspace, store, trust)
    });
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    (view, cx)
}

/// A library file a previous run would have left, with one command in it.
fn seed_snippet(dir: &TempDir, name: &str, body: &str) -> shellune_core::SnippetId {
    let mut book = shellune_core::SnippetBook::new();
    let id = book.create(name, body).expect("a valid command");
    shellune_storage::SnippetStore::at(dir.path())
        .save(&book)
        .expect("the library saves");
    id
}

fn snippet_sent(
    cx: &mut VisualTestContext,
    view: &Entity<ShelluneView>,
    session: SessionId,
) -> Vec<u8> {
    cx.update(|_window, app| {
        view.read(app)
            .pane_of(session)
            .map(|pane| pane.read(app).sent().to_vec())
            .unwrap_or_default()
    })
}

#[gpui::test]
async fn a_shortcut_command_is_saved_found_in_the_palette_and_read_back_after_a_restart(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-save");
    let (view, vcx) = snippet_view(cx, &dir);

    // Through the manager the palette offers: a name, a command, save.
    vcx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(true, cx));
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("snippet.editor").is_some(),
        "the editor is drawn when it is opened"
    );
    type_into(vcx, "snippet.editor.name", "deploy");
    type_into(vcx, "snippet.editor.body", "make deploy");
    click(vcx, "snippet.editor.save");
    assert_eq!(
        vcx.update(|_window, app| view.read(app).snippets().snippets().len()),
        1,
        "the command is in the library"
    );
    assert!(
        !vcx.update(|_window, app| view.read(app).snippet_panel().is_editing()),
        "the editor closes once the command is saved"
    );
    assert!(
        vcx.debug_bounds("snippet.row.0").is_some(),
        "the saved command is listed"
    );

    // The palette offers the two actions, named after the command.
    vcx.simulate_keystrokes(palette_keystroke());
    vcx.run_until_parked();
    vcx.simulate_input("deploy");
    vcx.run_until_parked();
    let titles = vcx.update(|_window, app| view.read(app).palette_titles());
    let insert = copy::fill(
        copy::ENGLISH_UX.snippet_insert_action,
        &[("name", "deploy")],
    );
    let run = copy::fill(copy::ENGLISH_UX.snippet_run_action, &[("name", "deploy")]);
    assert!(
        titles.iter().any(|title| title == &insert),
        "the palette can type the command: {titles:?}"
    );
    assert!(
        titles.iter().any(|title| title == &run),
        "the palette can run the command: {titles:?}"
    );
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    // A restart is a second window on the same directory.
    let (restarted, vcx) = snippet_view(cx, &dir);
    vcx.run_until_parked();
    let read: Vec<(String, String)> = vcx.update(|_window, app| {
        restarted
            .read(app)
            .snippets()
            .snippets()
            .iter()
            .map(|snippet| (snippet.name().to_owned(), snippet.body().to_owned()))
            .collect()
    });
    assert_eq!(
        read,
        vec![("deploy".to_owned(), "make deploy".to_owned())],
        "the next run reads the command the last one saved"
    );
}

#[gpui::test]
async fn the_editor_keeps_a_line_break_so_a_block_is_saved_as_a_block(cx: &mut TestAppContext) {
    let dir = TempDir::new("snippet-lines");
    let (view, cx) = snippet_view(cx, &dir);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(true, cx));
    });
    cx.run_until_parked();
    type_into(cx, "snippet.editor.name", "checks");
    click(cx, "snippet.editor.body");
    cx.simulate_input("cargo fmt");
    cx.simulate_keystrokes("shift-enter");
    cx.simulate_input("cargo test");
    cx.run_until_parked();
    click(cx, "snippet.editor.save");
    let saved = cx.update(|_window, app| {
        view.read(app)
            .snippets()
            .snippets()
            .first()
            .map(|snippet| snippet.body().to_owned())
    });
    assert_eq!(
        saved.as_deref(),
        Some("cargo fmt\ncargo test"),
        "the break typed with Shift+Enter is saved, not flattened into a space"
    );
    assert!(
        cx.debug_bounds("snippet.lines.0").is_some(),
        "a multi-line command says so in the list"
    );
}

#[gpui::test]
async fn a_command_with_no_terminal_explains_and_creates_nothing(cx: &mut TestAppContext) {
    let dir = TempDir::new("snippet-no-terminal");
    let id = seed_snippet(&dir, "deploy", "make deploy");
    let (view, cx) = snippet_view(cx, &dir);
    assert!(
        cx.update(|_window, app| view.read(app).open_sessions().is_empty()),
        "the window has no terminal on screen"
    );

    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(id, false, cx));
    });
    cx.run_until_parked();
    let toasts = cx.update(|_window, app| view.read(app).toast_titles());
    assert!(
        toasts
            .iter()
            .any(|title| title == copy::ENGLISH_UX.snippet_no_terminal),
        "the window says a terminal is needed first: {toasts:?}"
    );
    assert!(
        toasts
            .iter()
            .all(|title| title != copy::ENGLISH_UX.snippet_inserted
                && title != copy::ENGLISH_UX.snippet_sent),
        "nothing is reported as inserted or run: {toasts:?}"
    );
    assert!(
        cx.update(|_window, app| view.read(app).open_sessions().is_empty()),
        "no session is created for a command that has nowhere to go"
    );
}

#[gpui::test]
async fn filling_a_command_in_is_not_reported_as_running_it(cx: &mut TestAppContext) {
    let dir = TempDir::new("snippet-fill");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let id = seed_snippet(&dir, "release", "cargo build --release");
    let (view, cx) = snippet_view(cx, &dir);
    live_remote(cx, &view);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(id, false, cx));
    });
    cx.run_until_parked();
    let toasts = cx.update(|_window, app| view.read(app).toast_titles());
    assert!(
        toasts
            .iter()
            .any(|title| title == copy::ENGLISH_UX.snippet_inserted),
        "filling in says the command is in the terminal: {toasts:?}"
    );
    assert!(
        toasts
            .iter()
            .all(|title| title != copy::ENGLISH_UX.snippet_sent),
        "filling in never claims the command ran: {toasts:?}"
    );
}

#[gpui::test]
async fn a_multi_line_command_waits_in_the_dialog_and_is_reported_as_waiting(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-dialog");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let body = "cargo fmt\ncargo test";
    let id = seed_snippet(&dir, "checks", body);
    let (view, cx) = snippet_view(cx, &dir);
    let session = live_remote(cx, &view);
    let before = snippet_sent(cx, &view, session);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(id, true, cx));
    });
    cx.run_until_parked();

    assert_eq!(
        snippet_sent(cx, &view, session),
        before,
        "a held command is not in the terminal yet"
    );
    assert!(
        cx.debug_bounds("terminal.paste").is_some()
            && cx.debug_bounds("terminal.paste.runs").is_some(),
        "the dialog says the block runs once it is confirmed"
    );
    let toasts = cx.update(|_window, app| view.read(app).toast_titles());
    assert!(
        toasts
            .iter()
            .any(|title| title == copy::ENGLISH_UX.snippet_waiting),
        "the corner says the command waits rather than that it ran: {toasts:?}"
    );
    assert!(
        toasts
            .iter()
            .all(|title| title != copy::ENGLISH_UX.snippet_sent),
        "a command behind the dialog is never reported as sent: {toasts:?}"
    );

    click(cx, "terminal.paste.confirm");
    let sent = snippet_sent(cx, &view, session);
    let text = String::from_utf8_lossy(&sent).into_owned();
    assert!(
        text.contains("cargo fmt") && text.contains("cargo test"),
        "confirming writes both lines: {text:?}"
    );
    assert!(
        sent.ends_with(b"\r"),
        "running submits the block, so the last byte is a Return: {text:?}"
    );
    assert_eq!(
        sent.iter().filter(|byte| **byte == b'\r').count(),
        2,
        "the block keeps its own line break and running adds exactly one submit: {text:?}"
    );
}

#[gpui::test]
async fn running_a_command_submits_it_once_in_the_active_pane_only(cx: &mut TestAppContext) {
    let dir = TempDir::new("snippet-run");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let id = seed_snippet(&dir, "release", "cargo build --release");
    let (view, cx) = snippet_view(cx, &dir);
    let first = live_remote(cx, &view);
    let second = live_remote_second(cx, &view, first);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(second),
        "the newest session is the one on screen"
    );

    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(id, true, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        String::from_utf8_lossy(&snippet_sent(cx, &view, second)),
        "cargo build --release\r",
        "running types the command and submits it exactly once"
    );
    assert_eq!(
        snippet_sent(cx, &view, first),
        Vec::<u8>::new(),
        "the session beside it receives nothing"
    );

    // Inserting is the same write without the submit.
    let before = snippet_sent(cx, &view, second);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(id, false, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, second),
        [before, b"cargo build --release".to_vec()].concat(),
        "inserting appends the command and no line break of its own"
    );
    assert_eq!(
        snippet_sent(cx, &view, first),
        Vec::<u8>::new(),
        "the other pane is still untouched"
    );
}

#[gpui::test]
async fn the_manager_is_walkable_with_the_keyboard_and_enter_inserts_the_chosen_command(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-keys");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let mut book = shellune_core::SnippetBook::new();
    book.create("first", "echo one").expect("a valid command");
    book.create("second", "echo two").expect("a valid command");
    shellune_storage::SnippetStore::at(dir.path())
        .save(&book)
        .expect("the library saves");
    let (view, cx) = snippet_view(cx, &dir);
    let session = live_remote(cx, &view);

    cx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(false, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("snippet.row.0").is_some() && cx.debug_bounds("snippet.row.1").is_some(),
        "both saved commands are listed"
    );

    cx.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).snippet_panel().selected()),
        1,
        "the arrow key moves the highlighted row"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        String::from_utf8_lossy(&snippet_sent(cx, &view, session)),
        "echo two",
        "Enter inserts the highlighted command without submitting it"
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).tool_for_test()),
        None,
        "Escape closes the manager"
    );
}

#[gpui::test]
async fn a_multi_line_command_asks_before_it_is_written_and_cancelling_writes_nothing(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("", cx);
    let body = "cargo fmt\ncargo test";
    let offer =
        cx.update(|_window, app| pane.update(app, |pane, cx| pane.stage_snippet(body, true, cx)));
    cx.run_until_parked();
    assert_eq!(
        offer,
        SnippetOffer::Waiting,
        "the pane says nothing has been written yet, so the window cannot claim it was sent"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        Vec::<u8>::new(),
        "a multi-line block writes nothing before it is confirmed"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        Some(body.to_owned()),
        "the confirmation is the paste prompt, holding the same text"
    );
    assert!(
        cx.update(|_window, app| pane.read(app).pending_paste_runs()),
        "the prompt says the block runs once it is confirmed"
    );
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| pane.cancel_paste(cx));
    });
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        Vec::<u8>::new(),
        "cancelling writes nothing, so no line of it runs"
    );

    // The same block inserted rather than run: no submit, and the prompt says so.
    let offer =
        cx.update(|_window, app| pane.update(app, |pane, cx| pane.stage_snippet(body, false, cx)));
    cx.run_until_parked();
    assert_eq!(
        offer,
        SnippetOffer::Waiting,
        "an insert that is held is not reported as inserted either"
    );
    assert!(
        !cx.update(|_window, app| pane.read(app).pending_paste_runs()),
        "an insert does not claim it will run"
    );
    cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.confirm_paste(cx);
        });
    });
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        encode_paste(body, TermMode::empty()),
        "confirming an insert types the block and submits nothing of its own"
    );
}

#[gpui::test]
async fn inserting_a_command_does_not_submit_it_and_running_submits_it_once(
    cx: &mut TestAppContext,
) {
    let (pane, cx) = pane_with("", cx);
    // A body saved with a final newline must not submit itself when it is inserted.
    let offer = cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.stage_snippet("make deploy\n", false, cx)
        })
    });
    assert_eq!(
        offer,
        SnippetOffer::Written,
        "a plain line is in the pane as soon as it is offered"
    );
    assert_eq!(
        cx.update(|_window, app| pane.read(app).sent().to_vec()),
        b"make deploy".to_vec(),
        "inserting drops the stored line ending instead of sending it"
    );
    assert!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().is_none()),
        "a single line asks nothing"
    );

    let before = cx.update(|_window, app| pane.read(app).sent().to_vec());
    let offer = cx.update(|_window, app| {
        pane.update(app, |pane, cx| {
            pane.stage_snippet("make deploy\n\n", true, cx)
        })
    });
    assert_eq!(
        offer,
        SnippetOffer::Written,
        "running a single line writes it and asks nothing"
    );
    assert_eq!(
        String::from_utf8_lossy(&cx.update(|_window, app| pane.read(app).sent().to_vec())),
        "make deploymake deploy\r",
        "running adds exactly one line break after the command, never two"
    );
    assert!(!before.is_empty());
}

#[gpui::test]
async fn a_held_command_is_never_reported_as_sent_and_answering_the_prompt_settles_it(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-held");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let mut book = shellune_core::SnippetBook::new();
    let block = "cargo fmt\ncargo test";
    let held = book.create("checks", block).expect("a valid command");
    let plain = book
        .create("build", "cargo build")
        .expect("a valid command");
    shellune_storage::SnippetStore::at(dir.path())
        .save(&book)
        .expect("the library saves");
    let (view, cx) = snippet_view(cx, &dir);
    let session = live_remote(cx, &view);

    let claims =
        |cx: &mut VisualTestContext| cx.update(|_window, app| view.read(app).toast_entries());
    let claims_success = |entries: &[(Tone, String)]| {
        entries.iter().any(|(tone, title)| {
            *tone == Tone::Positive
                && (title == copy::ENGLISH_UX.snippet_sent
                    || title == copy::ENGLISH_UX.snippet_inserted)
        })
    };
    let waits = |entries: &[(Tone, String)]| {
        entries.iter().any(|(tone, title)| {
            *tone == Tone::Neutral && title == copy::ENGLISH_UX.snippet_waiting
        })
    };

    // Running a block asks in the pane first. The corner may point at that prompt, but it
    // may not say the command was sent while nothing has been written.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(held, true, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("terminal.paste").is_some()
            && cx
                .update(|_window, app| {
                    view.read(app)
                        .pane_of(session)
                        .and_then(|pane| pane.read(app).pending_paste_text().map(str::to_owned))
                })
                .is_some_and(|waiting| waiting == block),
        "the pane holds the block and asks before anything is written"
    );
    let entries = claims(cx);
    assert!(
        waits(&entries),
        "the corner says the command is waiting in the pane: {entries:?}"
    );
    assert!(
        !claims_success(&entries),
        "nothing claims a command reached the shell before it was confirmed: {entries:?}"
    );
    assert_eq!(
        snippet_sent(cx, &view, session),
        Vec::<u8>::new(),
        "not one line of the block is written before the answer"
    );

    // Choosing not to paste writes nothing, and leaves no note behind that says a command
    // is waiting for an answer that will never come.
    click(cx, "terminal.paste.cancel");
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, session),
        Vec::<u8>::new(),
        "cancelling writes nothing, so no line of it runs"
    );
    let entries = claims(cx);
    assert!(
        !claims_success(&entries),
        "a cancelled command is never reported as sent: {entries:?}"
    );
    assert!(
        !waits(&entries),
        "the note about the prompt leaves with the prompt: {entries:?}"
    );

    // Inserting the same block waits the same way, and confirming it types it once.
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(held, false, cx));
    });
    cx.run_until_parked();
    let entries = claims(cx);
    assert!(
        waits(&entries) && !claims_success(&entries),
        "a held insert is not reported as inserted either: {entries:?}"
    );
    click(cx, "terminal.paste.confirm");
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, session),
        encode_paste(block, TermMode::empty()),
        "confirming types the block and submits nothing of its own"
    );
    let entries = claims(cx);
    assert!(
        !waits(&entries),
        "the prompt was answered, so the note about it is gone: {entries:?}"
    );

    // A single line is written straight away, and only then is it reported as inserted.
    let before = snippet_sent(cx, &view, session);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(plain, false, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, session),
        [before, b"cargo build".to_vec()].concat(),
        "a plain line is in the pane without a question"
    );
    let entries = claims(cx);
    assert!(
        entries
            .iter()
            .any(|(tone, title)| *tone == Tone::Positive
                && title == copy::ENGLISH_UX.snippet_inserted),
        "a line that really was inserted says so: {entries:?}"
    );

    // The same holds for running one: it is written and submitted before it is reported.
    let before = snippet_sent(cx, &view, session);
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(plain, true, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, session),
        [before, b"cargo build\r".to_vec()].concat(),
        "running a plain line submits it once"
    );
    let entries = claims(cx);
    assert!(
        entries
            .iter()
            .any(|(tone, title)| *tone == Tone::Positive && title == copy::ENGLISH_UX.snippet_sent),
        "a line that really was sent says so: {entries:?}"
    );
}

#[gpui::test]
async fn a_damaged_library_is_named_and_never_written_over(cx: &mut TestAppContext) {
    let dir = TempDir::new("snippet-damaged");
    let broken = b"{not a library";
    std::fs::write(
        dir.path().join(shellune_storage::SNIPPETS_FILE_NAME),
        broken,
    )
    .expect("the damaged file is written");
    let (view, cx) = snippet_view(cx, &dir);
    assert!(
        !cx.update(|_window, app| view.read(app).snippets().can_write()),
        "an unreadable library is not written over"
    );
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(true, cx));
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("snippets.unreadable").is_some(),
        "the dialog names the file it could not read"
    );
    type_into(cx, "snippet.editor.name", "deploy");
    type_into(cx, "snippet.editor.body", "make deploy");
    click(cx, "snippet.editor.save");
    assert_eq!(
        std::fs::read(dir.path().join(shellune_storage::SNIPPETS_FILE_NAME))
            .expect("the file is still there"),
        broken.to_vec(),
        "the unreadable file is exactly as it was"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).snippets().snippets().len()),
        0,
        "nothing was added to a library that could not be read"
    );
    assert!(
        cx.debug_bounds("snippet.editor").is_some(),
        "the draft stays on screen while the file waits to be repaired"
    );
    assert!(
        cx.debug_bounds("session.notice").is_some(),
        "the reason the save did not happen is stated"
    );
}

#[gpui::test]
async fn deleting_a_command_asks_first_and_removes_it_from_the_file(cx: &mut TestAppContext) {
    let dir = TempDir::new("snippet-delete");
    seed_snippet(&dir, "deploy", "make deploy");
    let (view, vcx) = snippet_view(cx, &dir);
    vcx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(false, cx));
    });
    vcx.run_until_parked();
    click(vcx, "snippet.delete.0");
    assert!(
        vcx.debug_bounds("snippet.delete.question").is_some(),
        "deleting a saved command asks first"
    );
    assert!(
        vcx.update(|_window, app| view.read(app).snippet_panel().pending_delete().is_some()),
        "the question names the command it would remove"
    );
    click(vcx, "snippet.delete.no");
    assert_eq!(
        vcx.update(|_window, app| view.read(app).snippets().snippets().len()),
        1,
        "keeping the command leaves it in the library"
    );
    click(vcx, "snippet.delete.0");
    click(vcx, "snippet.delete.yes");
    assert_eq!(
        vcx.update(|_window, app| view.read(app).snippets().snippets().len()),
        0,
        "answering removes the command"
    );

    let (restarted, vcx) = snippet_view(cx, &dir);
    vcx.run_until_parked();
    assert!(
        vcx.update(|_window, app| restarted.read(app).snippets().snippets().is_empty()),
        "the file was written, so the next run has no such command"
    );
}

#[gpui::test]
async fn the_shortcut_command_dialog_stays_inside_the_minimum_window_at_200_percent(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-min");
    seed_snippet(
        &dir,
        "a rather long name for a saved command",
        "cargo fmt\ncargo test --workspace",
    );
    let (view, cx) = snippet_view(cx, &dir);
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(false, cx));
    });
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    // The dialog and its own controls stay on screen; the body between them scrolls, so a
    // row that does not fit is reached by scrolling rather than by losing its actions.
    for selector in [
        "snippets.dialog",
        "snippets.dialog.footer.close",
        "snippets.new",
        "modal.subtitle",
    ] {
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
    }
    for selector in [
        "snippet.row.0",
        "snippet.insert.0",
        "snippet.run.0",
        "snippets.storage",
    ] {
        assert_within_width(bounds_of(cx, selector), viewport, selector);
    }

    // The editor's fields fit across the window too, so a command can be written at this
    // text size without a field leaving the right edge. The footer's new-command button is
    // what opens it here: the rows themselves are reached by scrolling in this window.
    click(cx, "snippets.new");
    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_fits_inside(bounds_of(cx, "snippets.dialog"), viewport, "the dialog");
    for selector in [
        "snippet.editor.name",
        "snippet.editor.body",
        "snippet.editor.save",
    ] {
        assert_within_width(bounds_of(cx, selector), viewport, selector);
    }
    // The editor is taller than the body at this text size, so its fields and the button
    // that saves them are reached by scrolling the body rather than clipped by it.
    for selector in [
        "snippet.editor.name",
        "snippet.editor.body",
        "snippet.editor.save",
    ] {
        scroll_into_view(cx, "snippets.dialog.body", selector);
        assert_fits_inside(bounds_of(cx, selector), viewport, selector);
        assert_pointer_target(cx, selector);
    }
}

/// Wheels a scrollable box by one line, with the pointer over the box.
fn wheel_down(cx: &mut VisualTestContext, container: &'static str) {
    let at = bounds_of(cx, container).center();
    cx.simulate_mouse_move(at, None, Modifiers::default());
    cx.simulate_event(gpui::ScrollWheelEvent {
        position: at,
        delta: gpui::ScrollDelta::Lines(point(0.0, -1.0)),
        ..Default::default()
    });
    cx.run_until_parked();
}

/// Wheels a scrollable box until `target` is wholly inside the part of it that is on
/// screen.
///
/// A pane one slice tall can only show a card that is taller than itself, so "the whole
/// control is reachable" means the wheel brings all of it into the box, not just its
/// centre (R4).
fn scroll_fully_into_view(
    cx: &mut VisualTestContext,
    container: &'static str,
    target: &'static str,
) {
    for _ in 0..80 {
        let outer = bounds_of(cx, container);
        let bounds = bounds_of(cx, target);
        let top = outer.origin.y - px(0.5);
        let bottom = outer.origin.y + outer.size.height + px(0.5);
        if bounds.origin.y >= top && bounds.origin.y + bounds.size.height <= bottom {
            return;
        }
        assert!(
            bounds.origin.y + bounds.size.height > top,
            "{target} scrolled past {container} without ever being inside it"
        );
        wheel_down(cx, container);
    }
    panic!("{target} never fit inside {container}");
}

/// Wheels a scrollable box until the centre of `target` is inside the part of it that is
/// on screen, the way a person scrolls down to a control below the fold.
///
/// In the minimum window at 200% text a dialog shows one slice of its body, so where an
/// element was laid out says nothing about whether anyone can reach it (R4): the question
/// is whether scrolling reaches it, and this is that question asked of the drawing itself.
fn scroll_into_view(cx: &mut VisualTestContext, container: &'static str, target: &'static str) {
    for _ in 0..80 {
        let outer = bounds_of(cx, container);
        let center = bounds_of(cx, target).center();
        let inside_y = center.y >= outer.origin.y - px(0.5)
            && center.y <= outer.origin.y + outer.size.height + px(0.5);
        let inside_x = center.x >= outer.origin.x - px(0.5)
            && center.x <= outer.origin.x + outer.size.width + px(0.5);
        if inside_x && inside_y {
            return;
        }
        assert!(
            center.y >= outer.origin.y - px(0.5),
            "{target} scrolled past {container} without ever being inside it"
        );
        wheel_down(cx, container);
    }
    panic!("{target} never came into view inside {container}");
}

#[gpui::test]
async fn a_saved_command_row_keeps_a_readable_preview_and_reaches_its_buttons_at_200_percent(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-reached");
    seed_snippet(
        &dir,
        "a rather long name for a saved command",
        "cargo fmt\ncargo test --workspace",
    );
    let (view, cx) = snippet_view(cx, &dir);
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    cx.update(|_window, app| {
        view.update(app, |view, cx| view.open_snippets(false, cx));
    });
    cx.run_until_parked();

    let metrics = Metrics::new(cx.update(|_window, app| view.read(app).workspace().zoom()));
    let viewport = cx.update(|window, _app| window.viewport_size());
    // The row is one line of name and one line of body, not a column of single
    // characters: at this text size the four action labels are wider than the row, and a
    // name that is allowed to wrap instead is squeezed to a few pixels and drawn one
    // character per line — a 2716 px row inside a 119 px body.
    let row = bounds_of(cx, "snippet.row.0");
    let preview = bounds_of(cx, "snippet.preview.0");
    assert_within_width(row, viewport, "the row");
    assert!(
        preview.size.width >= metrics.scale(120.0),
        "the body preview keeps a readable part of the row: {preview:?}"
    );
    assert!(
        preview.size.height <= metrics.caption_size() * 2.0,
        "the preview is one line of text rather than a ribbon of characters: {preview:?}"
    );
    assert!(
        row.size.height <= metrics.scale(140.0),
        "one saved command stays a row, not a column taller than the dialog: {row:?}"
    );

    // Every action of the row is on the card, and the ones below the fold are reached by
    // scrolling the body rather than lost.
    let dialog = bounds_of(cx, "snippets.dialog");
    for selector in [
        "snippet.insert.0",
        "snippet.run.0",
        "snippet.edit.0",
        "snippet.delete.0",
    ] {
        assert_inside_horizontally(bounds_of(cx, selector), dialog, selector);
        assert_pointer_target(cx, selector);
    }

    // The same holds in Chinese, where the four labels are full-width words.
    cx.dispatch_action(ToggleLanguage);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().locale()),
        Locale::Chinese,
        "the row is measured in Chinese too"
    );
    let preview = bounds_of(cx, "snippet.preview.0");
    assert!(
        preview.size.width >= metrics.scale(120.0),
        "the preview keeps its part of the row in Chinese: {preview:?}"
    );
    assert!(
        preview.size.height <= metrics.caption_size() * 2.0,
        "the preview stays one line in Chinese: {preview:?}"
    );
    for selector in [
        "snippet.insert.0",
        "snippet.run.0",
        "snippet.edit.0",
        "snippet.delete.0",
    ] {
        assert_inside_horizontally(bounds_of(cx, selector), dialog, selector);
        assert_pointer_target(cx, selector);
    }

    scroll_into_view(cx, "snippets.dialog.body", "snippet.delete.0");
    assert_fits_inside(
        bounds_of(cx, "snippet.delete.0"),
        viewport,
        "the delete button once scrolled to",
    );

    // The question it opens is reached the same way, and its answer is a real one.
    click(cx, "snippet.delete.0");
    assert!(
        cx.debug_bounds("snippet.delete.question").is_some(),
        "opening the question draws it"
    );
    scroll_into_view(cx, "snippets.dialog.body", "snippet.delete.no");
    for selector in ["snippet.delete.yes", "snippet.delete.no"] {
        assert_inside_horizontally(bounds_of(cx, selector), dialog, selector);
        assert_pointer_target(cx, selector);
    }
    click(cx, "snippet.delete.no");
    assert_eq!(
        cx.update(|_window, app| view.read(app).snippets().snippets().len()),
        1,
        "answering no keeps the command in the library"
    );
}

#[gpui::test]
async fn the_palette_reaches_the_saved_command_rows_in_the_minimum_window_at_200_percent(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-palette");
    seed_snippet(
        &dir,
        "deploy the release",
        "cargo fmt\ncargo test --workspace",
    );
    let (view, cx) = snippet_view(cx, &dir);
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    go(cx, crate::app::actions::OpenPalette);
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    let palette = bounds_of(cx, "palette");
    assert_fits_inside(palette, viewport, "the palette");

    // The palette is longer than its list box at this text size. The rows below the fold
    // are reached by scrolling the list, and the panel itself does not move or grow while
    // they are: a panel that left the window would take the list with it (R4).
    let entries = cx.update(|_window, app| view.read(app).palette_titles());
    assert!(
        entries.len() > 3,
        "the palette has more entries than its box shows: {entries:?}"
    );
    let last_selector: &'static str =
        Box::leak(format!("palette.item.{}", entries.len() - 1).into_boxed_str());
    scroll_into_view(cx, "palette.list", last_selector);
    let list = bounds_of(cx, "palette.list");
    let last = bounds_of(cx, last_selector);
    assert_inside_horizontally(last, palette, last_selector);
    assert!(
        last.center().y >= list.origin.y - px(0.5)
            && last.center().y <= list.origin.y + list.size.height + px(0.5),
        "the last row was scrolled into the list's own box: {last:?} in {list:?}"
    );
    assert_fits_inside(
        bounds_of(cx, "palette"),
        viewport,
        "the palette after scrolling",
    );

    // Searching by the command's name leaves the pair the palette offers for it — type it,
    // or run it — and both rows are drawn inside the list's box rather than clipped by it.
    cx.simulate_input("deploy");
    cx.run_until_parked();
    let titles = cx.update(|_window, app| view.read(app).palette_titles());
    assert_eq!(
        titles.len(),
        2,
        "the saved command is found by its name: {titles:?}"
    );
    assert!(
        titles
            .iter()
            .all(|title| title.contains("deploy the release")),
        "both entries name the command they are about: {titles:?}"
    );
    let list = bounds_of(cx, "palette.list");
    for index in 0..titles.len() {
        let selector: &'static str = Box::leak(format!("palette.item.{index}").into_boxed_str());
        let row = bounds_of(cx, selector);
        assert_inside_horizontally(row, palette, selector);
        assert!(
            row.origin.y >= list.origin.y - px(0.5)
                && row.origin.y + row.size.height <= list.origin.y + list.size.height + px(0.5),
            "the row of the saved command is inside the list's box: {row:?} in {list:?}"
        );
    }
}

#[gpui::test]
async fn a_terminal_whose_shell_ended_reports_that_instead_of_a_command_being_inserted(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("snippet-ended");
    seed(
        &dir.store(),
        vec![stored_host("web", "10.0.0.5", 22, "deploy", None)],
    );
    let id = seed_snippet(&dir, "deploy", "make deploy");
    let (view, cx) = snippet_view(cx, &dir);
    let session = live_remote(cx, &view);
    // An ended shell keeps its last screen and takes no more input.
    cx.update(|_window, app| {
        let pane = view
            .read(app)
            .pane_of(session)
            .expect("the session has a pane")
            .clone();
        pane.update(app, |pane, cx| {
            pane.adopt_events(pane.tag(), vec![SourceEvent::Exited(ExitReport::Clean)], cx);
        });
    });
    cx.run_until_parked();
    let before = snippet_sent(cx, &view, session);

    cx.update(|_window, app| {
        view.update(app, |view, cx| view.use_snippet(id, false, cx));
    });
    cx.run_until_parked();
    assert_eq!(
        snippet_sent(cx, &view, session),
        before,
        "nothing is typed into a shell that has ended"
    );
    let toasts = cx.update(|_window, app| view.read(app).toast_titles());
    assert!(
        toasts
            .iter()
            .any(|title| title == copy::ENGLISH_UX.snippet_terminal_ended),
        "the window says the terminal has no shell running: {toasts:?}"
    );
    assert!(
        toasts
            .iter()
            .all(|title| title != copy::ENGLISH_UX.snippet_inserted),
        "nothing is reported as inserted: {toasts:?}"
    );
}

#[gpui::test]
async fn the_host_dialog_saves_the_chosen_x11_mode(cx: &mut TestAppContext) {
    let (view, cx, dir) = dialog_view(cx, vec![stored_host("web", "127.0.0.1", 1, "deploy", None)]);
    // The host form is taller than the design window, and a test clicks what a user
    // reaches by scrolling.
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "hosts.edit.0");
    assert!(
        cx.debug_bounds("hosts.editor.x11").is_some(),
        "the X11 mode is part of the host form"
    );
    click(cx, "hosts.editor.x11.restricted");
    assert!(
        cx.debug_bounds("hosts.editor.x11.warning").is_none(),
        "the restricted mode carries no risk sentence"
    );
    click(cx, "hosts.save");

    let host = cx
        .update(|_window, app| view.read(app).book().hosts().first().cloned())
        .expect("the host was saved");
    assert_eq!(
        host.x11(),
        X11Forwarding::Restricted,
        "the chosen mode is part of the saved host"
    );
    let file = std::fs::read_to_string(dir.path().join(shellune_storage::FILE_NAME))
        .expect("the host file was written");
    assert!(
        file.contains("Restricted"),
        "the saved mode reaches the host file: {file}"
    );

    // The trusted mode says what it allows before it is chosen, and off is the
    // default that is not written as a field at all.
    click(cx, "hosts.edit.0");
    click(cx, "hosts.editor.x11.trusted");
    assert!(
        cx.debug_bounds("hosts.editor.x11.warning").is_some(),
        "the trusted mode shows the risk it carries"
    );
    click(cx, "hosts.save");
    let host = cx
        .update(|_window, app| view.read(app).book().hosts().first().cloned())
        .expect("the host was saved");
    assert_eq!(host.x11(), X11Forwarding::Trusted);

    click(cx, "hosts.edit.0");
    click(cx, "hosts.editor.x11.off");
    click(cx, "hosts.save");
    let host = cx
        .update(|_window, app| view.read(app).book().hosts().first().cloned())
        .expect("the host was saved");
    assert_eq!(
        host.x11(),
        X11Forwarding::Off,
        "choosing off again is what a host without a choice means"
    );
    let file = std::fs::read_to_string(dir.path().join(shellune_storage::FILE_NAME))
        .expect("the host file was written");
    assert!(
        !file.contains("\"x11\""),
        "the default is not stored as a field: {file}"
    );
}

#[gpui::test]
async fn a_trusted_x11_connection_asks_before_it_dials(cx: &mut TestAppContext) {
    let (view, cx, _dir) =
        dialog_view(cx, vec![stored_host("web", "127.0.0.1", 1, "deploy", None)]);
    // The connection dialog has to show its whole form for a test to click the parts
    // of it that a small window puts below the fold.
    cx.simulate_resize(tall_window());
    cx.run_until_parked();
    click(cx, "hosts.connect.0");
    let mode = cx.update(|_window, app| {
        view.read(app)
            .remote_state()
            .expect("the form is open")
            .x11()
    });
    assert_eq!(
        mode,
        X11Forwarding::Off,
        "a host saved without a choice forwards nothing"
    );

    // Changing the mode here is for this connection only.
    click(cx, "remote.x11.trusted");
    assert!(
        cx.debug_bounds("remote.x11.warning").is_some(),
        "the form says what the trusted mode allows"
    );
    type_into(cx, "remote.password", "hunter2");
    click(cx, "remote.connect");
    assert!(
        cx.debug_bounds("remote.x11").is_some(),
        "the risks are asked about before anything is dialled"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0,
        "nothing is connected until the question is answered"
    );

    // Closing the question connects nothing and keeps the mode out of the record.
    click(cx, "remote.x11.answer.cancel");
    assert!(
        cx.update(|_window, app| view
            .read(app)
            .remote_state()
            .is_some_and(|panel| panel.status().x11_question().is_none())),
        "the question is gone once it was answered"
    );
    assert_eq!(
        cx.update(|_window, app| view.read(app).book().hosts()[0].x11()),
        X11Forwarding::Off,
        "a per-connection choice does not change the saved host"
    );

    // The question comes back for the next attempt; accepting it dials once.
    type_into(cx, "remote.password", "hunter2");
    click(cx, "remote.connect");
    assert!(
        cx.debug_bounds("remote.x11").is_some(),
        "every trusted attempt asks again"
    );
    click(cx, "remote.x11.answer.trusted");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        1,
        "accepting the risks starts the attempt that uses them"
    );
    // Closing the dialog ends that attempt, as closing any connecting form does.
    click(cx, "remote.close");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        0
    );
}

#[gpui::test]
async fn the_session_bar_says_what_x11_forwarding_is_doing(cx: &mut TestAppContext) {
    let (view, cx, _dir) =
        dialog_view(cx, vec![stored_host("web", "127.0.0.1", 1, "deploy", None)]);

    // A session that forwards: the bar says it is ready, and only then does it say
    // that a remote program can reach this display.
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", "hunter2");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::X11(X11Status::Enabling),
                    shellune_ssh::RemoteEvent::X11(X11Status::Ready {
                        display: ":96".to_owned(),
                    }),
                    shellune_ssh::RemoteEvent::Connected,
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session.x11.ready").is_some(),
        "the bar reports a display that is available"
    );
    assert!(
        cx.debug_bounds("session.x11.enabling").is_none(),
        "the newest status is the one on the bar"
    );
    assert!(
        cx.debug_bounds("session.x11.notice").is_none(),
        "a working bridge has nothing to explain"
    );

    // A second session whose bridge failed keeps its shell and says why the display
    // is not available.
    click(cx, "nav.hosts");
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", "hunter2");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::X11(X11Status::Failed {
                        reason: "no local X display is available: DISPLAY is not set".to_owned(),
                        next_step: "Start an X server for this computer.".to_owned(),
                    }),
                    shellune_ssh::RemoteEvent::Connected,
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session.x11.failed").is_some(),
        "the bar reports a display that could not be used"
    );
    // The reason and the next step are on the screen, not only in a tooltip: this test
    // never moved the pointer, which is what a keyboard-only reader depends on (R4).
    assert!(
        cx.debug_bounds("session.x11.notice").is_some(),
        "a failed bridge says so in text under the bar"
    );
    assert!(
        cx.debug_bounds("session.x11.reason").is_some(),
        "the reason is readable without a pointer"
    );
    assert!(
        cx.debug_bounds("session.x11.next").is_some(),
        "the next step is readable without a pointer"
    );
    assert_eq!(
        cx.update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .filter(|session| matches!(session.state(), SessionState::Connected))
                .count()
        }),
        2,
        "a failed display bridge leaves both shells connected"
    );

    // A session that forwards nothing says that as well: the bar does not leave the
    // reader guessing whether a display channel was asked for.
    click(cx, "nav.hosts");
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", "hunter2");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::X11(X11Status::Off),
                    shellune_ssh::RemoteEvent::Connected,
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session.x11.off").is_some(),
        "the bar says that this session forwards nothing"
    );
}

/// The X11 state, its reason and the session's actions in the smallest window (R4, R11).
///
/// The chip sits at the right of the session bar, after the identity and the target, in
/// the same row as the buttons that belong to the session; the banner that repeats the
/// reason and the next step hangs under that row. At 720×480 with 200% text the row has
/// no room to spare, so this measures that neither the state nor the text a keyboard user
/// reads instead of the tooltip is pushed past the window's edge, and that the session's
/// own close button stays inside it too.
#[gpui::test]
async fn the_x11_state_and_its_reason_stay_inside_the_minimum_window_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (view, cx, _dir) =
        dialog_view(cx, vec![stored_host("web", "127.0.0.1", 1, "deploy", None)]);
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", "hunter2");
    // The failure the restricted mode reports where the display refuses the authorization
    // it issued: the longest reason and next step of this round, so the banner is measured
    // with the text it really carries.
    let failure = shellune_ssh::X11Failure::RestrictedUnsupportedByDisplay {
        display: "127.0.0.1:96".to_owned(),
        detail: "Invalid MIT-MAGIC-COOKIE-1 key".to_owned(),
    };
    let (reason, next_step) = (failure.to_string(), failure.next_step());
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::X11(X11Status::Failed {
                        reason: reason.clone(),
                        next_step: next_step.clone(),
                    }),
                    shellune_ssh::RemoteEvent::Connected,
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session.x11.failed").is_some(),
        "the bar reports the refused display before the window is made small"
    );
    // In an ordinary window the same text is shown whole: the bound only takes over when
    // the window and the text size cannot both have their way.
    assert!(
        bounds_of(cx, "session.x11.notice").size.height
            <= bounds_of(cx, "session.x11.notice.body").size.height + px(0.5),
        "the explanation scrolled in a window that has room for all of it"
    );

    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().zoom()),
        Zoom::Largest,
        "200% text is what this measures"
    );

    let viewport = cx.update(|window, _app| window.viewport_size());
    // The state itself stays on the bar, in the row of the session's own actions,
    // and it does not paint over the session name (R4).
    assert_within_width(
        bounds_of(cx, "session.x11.failed"),
        viewport,
        "session.x11.failed",
    );
    let target = bounds_of(cx, "session.target");
    let chip = bounds_of(cx, "session.x11.failed");
    assert_fits_inside(target, viewport, "session.target");
    assert_fits_inside(chip, viewport, "session.x11.failed");
    assert!(
        chip.origin.x >= target.origin.x + target.size.width - px(1.0)
            || chip.origin.y >= target.origin.y + target.size.height - px(1.0),
        "the X11 state overlaps the session target: {chip:?} vs {target:?}"
    );
    // The explanation is bounded by the room the window has, so it cannot be drawn past
    // the bottom edge with the terminal behind it.
    let boxed = bounds_of(cx, "session.x11.notice.body");
    assert_fits_inside(boxed, viewport, "the X11 notice box");
    assert!(
        boxed.size.height <= viewport.height * 0.48,
        "the notice took more than its share of the window: {boxed:?} in {viewport:?}"
    );
    // The next step opens the box. The short failure's reason also fits this share;
    // a longer one is reached by scrolling, including with Page Up/Down once the box
    // has the keyboard (R4, R11).
    let next = bounds_of(cx, "session.x11.next");
    assert_inside_box(next, boxed, "the next step");
    // The whole direction does not fit this box at this size, and that is the contract and
    // not an oversight: the box is capped at `X11_NOTICE_SHARE` of the window, so the first
    // screen holds its opening and the rest of it, like the reason behind it, is reached by
    // scrolling. `assert_inside_box` only compares the width; the height a reader gets
    // before scrolling is what `first_screen` below measures.
    let one_line = Metrics::new(Zoom::Largest).label_size();
    // The banner's own padding is the only thing above it, so the next step is the first
    // text in the box rather than something the reason was put in front of.
    assert!(
        next.origin.y - boxed.origin.y <= one_line,
        "the next step does not open the notice box: {next:?} in {boxed:?}"
    );
    let reason_first = bounds_of(cx, "session.x11.reason");
    assert!(
        reason_first.origin.y >= next.origin.y + next.size.height - px(0.5),
        "the reason is not placed after the next step: {reason_first:?} after {next:?}"
    );
    let first_screen = (boxed.origin.y + boxed.size.height).min(next.origin.y + next.size.height)
        - next.origin.y.max(boxed.origin.y);
    // The first screen has to hold a usable opening of the direction — four of its lines at
    // this size, which is more than the single line a one-line direction needs — for putting
    // it first to mean anything to a reader who cannot scroll.
    assert!(
        first_screen >= one_line * 4.0,
        "the next step shows too little before any scrolling: \
         {first_screen:?} of {one_line:?} per line in {boxed:?}"
    );
    // The reason is the rest of the same text. When it does not fit the first screen,
    // the keyboard reaches it: focus the box and page down, which is the same contract
    // as the wheel (R4).
    cx.update(|window, app| {
        view.update(app, |view, _cx| view.focus_x11_notice_for_test(window));
    });
    cx.run_until_parked();
    press_key(cx, "home");
    for _ in 0..12 {
        let outer = bounds_of(cx, "session.x11.notice.body");
        let bounds = bounds_of(cx, "session.x11.reason");
        let top = outer.origin.y - px(0.5);
        let bottom = outer.origin.y + outer.size.height + px(0.5);
        if bounds.origin.y >= top && bounds.origin.y + bounds.size.height <= bottom {
            break;
        }
        press_key(cx, "pagedown");
    }
    scroll_fully_into_view(cx, "session.x11.notice.body", "session.x11.reason");
    let boxed = bounds_of(cx, "session.x11.notice.body");
    let reason = bounds_of(cx, "session.x11.reason");
    assert_inside_box(reason, boxed, "the reason");
    assert!(
        reason.origin.y >= boxed.origin.y - px(0.5)
            && reason.origin.y + reason.size.height <= boxed.origin.y + boxed.size.height + px(0.5),
        "the reason is not wholly inside the box it was scrolled into: {reason:?} in {boxed:?}"
    );
    // A bounded notice is only worth it if the session it belongs to is usable: the
    // terminal keeps a real height under it, and the actions still fit in the window.
    let terminal = bounds_of(cx, "terminal.viewport");
    assert!(
        terminal.size.height >= px(24.0),
        "the terminal has no room left under the notice: {terminal:?}"
    );
    assert_fits_inside(terminal, viewport, "the terminal");
    assert_fits_inside(bounds_of(cx, "session.close"), viewport, "session.close");
}

/// The failure a real window shows when DISPLAY is unset: the reason line is whole
/// on the first screen at 720×480 and 200% text, not cut by the banner edge (R4).
#[gpui::test]
async fn the_short_x11_reason_stays_on_the_first_screen_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (view, cx, _dir) =
        dialog_view(cx, vec![stored_host("web", "127.0.0.1", 1, "deploy", None)]);
    click(cx, "hosts.connect.0");
    type_into(cx, "remote.password", "hunter2");
    let reason = "no local X display is available: DISPLAY is not set";
    let next_step = "Start an X server for this computer and make sure DISPLAY names it, or connect without X11 forwarding.";
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::X11(X11Status::Failed {
                        reason: reason.to_owned(),
                        next_step: next_step.to_owned(),
                    }),
                    shellune_ssh::RemoteEvent::Connected,
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();
    cx.simulate_resize(minimum_window());
    focus_root(&view, cx);
    for _ in 0..Zoom::ALL.len() {
        if cx.update(|_window, app| view.read(app).workspace().zoom()) == Zoom::Largest {
            break;
        }
        cx.dispatch_action(CycleTextSize);
        cx.run_until_parked();
    }
    let viewport = cx.update(|window, _app| window.viewport_size());
    let boxed = bounds_of(cx, "session.x11.notice.body");
    let reason_bounds = bounds_of(cx, "session.x11.reason");
    let next = bounds_of(cx, "session.x11.next");
    assert_fits_inside(boxed, viewport, "the X11 notice");
    assert_fits_inside(next, viewport, "the next step");
    assert_fits_inside(reason_bounds, viewport, "the reason");
    assert!(
        reason_bounds.origin.y >= boxed.origin.y - px(0.5)
            && reason_bounds.origin.y + reason_bounds.size.height
                <= boxed.origin.y + boxed.size.height + px(0.5),
        "the reason is not whole on the first screen: {reason_bounds:?} in {boxed:?}"
    );
    let target = bounds_of(cx, "session.target");
    let chip = bounds_of(cx, "session.x11.failed");
    assert!(
        chip.origin.x >= target.origin.x + target.size.width - px(1.0)
            || chip.origin.y >= target.origin.y + target.size.height - px(1.0),
        "the X11 state overlaps the session target: {chip:?} vs {target:?}"
    );
}

/// A failed transfer's reason and Retry stay inside 720×480 at 200% text (R4).
#[gpui::test]
async fn a_failed_transfer_shows_its_reason_and_retry_in_the_minimum_window(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    let id = live_remote(cx, &view);
    let (_files, transfers) = open_transfers(cx, &view, id);
    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let started = transfers.starts();
    let task = started[0].transfer;
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Failed {
            failure: shellune_core::TransferFailure::NotFound,
            residue: None,
            resume: None,
        },
    });
    pump_files(cx, &view);
    cx.simulate_resize(minimum_window());
    largest_text_size(&view, cx);
    cx.run_until_parked();
    let viewport = minimum_window();
    let strip = bounds_of(cx, "files.transfers");
    assert_fits_inside(strip, viewport, "the transfer strip");
    assert_transfer_title_clears_the_scroll_clip(cx);
    cx.update(|window, app| {
        view.update(app, |view, _cx| view.focus_transfer_strip_for_test(window));
    });
    cx.run_until_parked();
    press_key(cx, "home");
    for selector in ["transfers.state.0", "transfers.retry.0"] {
        for _ in 0..12 {
            let outer = bounds_of(cx, "files.transfers");
            let bounds = bounds_of(cx, selector);
            let top = outer.origin.y - px(0.5);
            let bottom = outer.origin.y + outer.size.height + px(0.5);
            if bounds.origin.y >= top && bounds.origin.y + bounds.size.height <= bottom {
                break;
            }
            press_key(cx, "pagedown");
        }
        let bounds = bounds_of(cx, selector);
        let outer = bounds_of(cx, "files.transfers");
        assert!(
            bounds.origin.y >= outer.origin.y - px(0.5)
                && bounds.origin.y + bounds.size.height
                    <= outer.origin.y + outer.size.height + px(0.5),
            "{selector} is not inside the transfer strip after keyboard scrolling: \
             {bounds:?} in {outer:?}"
        );
        assert_fits_inside(bounds, viewport, selector);
    }
}

/// A failed transfer stays readable in the minimum window while X11 also failed.
///
/// The notice under the session bar is tall enough for the short X11 reason. Leaving
/// it there with files open pushes Retry past the client edge, so the body yields
/// while the file pane is up and the chip stays (R4).
#[gpui::test]
async fn a_transfer_failure_stays_whole_while_x11_failed_at_two_hundred_percent(
    cx: &mut TestAppContext,
) {
    let (view, cx) = remote_view(cx);
    click(cx, "nav.hosts");
    click(cx, "hosts.connect.0");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.script_remote_attempt(
                vec![
                    shellune_ssh::RemoteEvent::X11(X11Status::Failed {
                        reason: "no local X display is available: DISPLAY is not set".to_owned(),
                        next_step: "Start an X server for this computer and make sure DISPLAY names it, or connect without X11 forwarding.".to_owned(),
                    }),
                    shellune_ssh::RemoteEvent::Connected,
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    cx.run_until_parked();
    let id = cx
        .update(|_window, app| {
            view.read(app)
                .workspace()
                .sessions()
                .items()
                .iter()
                .find(|session| {
                    session.kind().is_remote() && matches!(session.state(), SessionState::Connected)
                })
                .map(|session| session.id())
        })
        .expect("the remote session is live");
    show_remote_terminal(cx, &view, id);
    let (_files, transfers) = open_transfers(cx, &view, id);
    click(cx, "files.upload");
    cx.update(|_window, app| {
        view.update(app, |view, cx| {
            view.files_question_set_text_for_test("C:/work/report.txt");
            view.submit_files_change_for_test(cx);
        });
    });
    cx.run_until_parked();
    let task = transfers.starts()[0].transfer;
    transfers.push(shellune_core::TransferEvent {
        transfer: task,
        update: shellune_core::TransferUpdate::Failed {
            failure: shellune_core::TransferFailure::NotFound,
            residue: None,
            resume: None,
        },
    });
    pump_files(cx, &view);
    cx.simulate_resize(minimum_window());
    largest_text_size(&view, cx);
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("session.x11.failed").is_some(),
        "the bar still says the bridge failed"
    );
    assert_transfer_title_clears_the_scroll_clip(cx);
    let viewport = cx.update(|window, _app| window.viewport_size());
    // The title now sits above the transfer scroller. That must not push the failure
    // chip out of this window; the reason and the next step are not drawn while files
    // are open, so the chip is the failure a reader still has (R4, R11).
    assert_fits_inside(
        bounds_of(cx, "session.x11.failed"),
        viewport,
        "session.x11.failed",
    );
    let scroll = bounds_of(cx, "files.transfers.scroll");
    for selector in ["transfers.state.0", "transfers.retry.0"] {
        let bounds = bounds_of(cx, selector);
        let outer = bounds_of(cx, "files.transfers");
        assert!(
            bounds.origin.y >= outer.origin.y - px(0.5)
                && bounds.origin.y + bounds.size.height
                    <= outer.origin.y + outer.size.height + px(0.5),
            "{selector} is clipped inside the transfer strip: {bounds:?} in {outer:?}"
        );
        assert!(
            bounds.origin.y >= scroll.origin.y - px(0.5)
                && bounds.origin.y + bounds.size.height
                    <= scroll.origin.y + scroll.size.height + px(0.5),
            "{selector} is clipped inside the transfer scroller: {bounds:?} in {scroll:?}"
        );
        assert_fits_inside(bounds, viewport, selector);
    }
    assert_fits_inside(bounds_of(cx, "session.close"), viewport, "session.close");
}
