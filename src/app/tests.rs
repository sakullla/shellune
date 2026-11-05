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
    ScrollWheelEvent, Size, TestAppContext, VisualTestContext, div, point, px, size,
};
use shellune_core::{
    HostConfig, LocalShell, Session, SessionKind, SessionState, Transfer, TransferDirection,
};
use shellune_storage::{HostBook, HostEdit, HostStore};
use shellune_terminal::GridSize;

use crate::app::actions::{CycleTextSize, GoToHosts, GoToSessions, GoToTransfers, ToggleLanguage};
use crate::app::copy::{self, CHINESE, ENGLISH, Locale, Strings};
use crate::app::hosts::HostView;
use crate::app::nav::{Destination, Platform};
use crate::app::product;
use crate::app::terminal::{PaneState, TerminalPane};
use crate::app::theme::{
    self, BODY_CONTRAST_FLOOR, GRAPHIC_CONTRAST_FLOOR, Metrics, Tone, Zoom, palette,
};
use crate::app::workspace::{Failure, PrimaryOutcome, Recovery, Section, SectionState, Workspace};
use crate::app::{DEFAULT_WINDOW_SIZE, MIN_WINDOW_SIZE, ShelluneView, key_bindings};

fn design_window() -> Size<Pixels> {
    size(px(DEFAULT_WINDOW_SIZE.0), px(DEFAULT_WINDOW_SIZE.1))
}

fn minimum_window() -> Size<Pixels> {
    size(px(MIN_WINDOW_SIZE.0), px(MIN_WINDOW_SIZE.1))
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
// Rendered layout
// ---------------------------------------------------------------------------

#[gpui::test]
async fn every_navigation_entry_is_measured_inside_the_design_window(cx: &mut TestAppContext) {
    let (_view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    for destination in Destination::ALL {
        let bounds = bounds_of(cx, destination.debug_selector());
        assert_fits_inside(bounds, viewport, destination.debug_selector());
    }
    assert_fits_inside(bounds_of(cx, "header.title"), viewport, "header title");
    assert_fits_inside(bounds_of(cx, "header.primary"), viewport, "primary action");
    assert_fits_inside(bounds_of(cx, "content"), viewport, "content panel");
}

#[gpui::test]
async fn the_minimum_window_still_shows_the_navigation_and_the_current_object(
    cx: &mut TestAppContext,
) {
    let (_view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_eq!(
        viewport,
        minimum_window(),
        "the test window is not the design minimum"
    );
    for destination in Destination::ALL {
        let bounds = bounds_of(cx, destination.debug_selector());
        assert_fits_inside(bounds, viewport, destination.debug_selector());
    }
    assert_fits_inside(bounds_of(cx, "content"), viewport, "content panel");
    assert_fits_inside(bounds_of(cx, "state.card"), viewport, "state card");
}

#[gpui::test]
async fn the_largest_text_size_keeps_every_entry_inside_the_minimum_window(
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
        "one step from the default must reach the largest text size"
    );
    let viewport = cx.update(|window, _app| window.viewport_size());
    for destination in Destination::ALL {
        let bounds = bounds_of(cx, destination.debug_selector());
        assert_fits_inside(bounds, viewport, destination.debug_selector());
    }
    assert_fits_inside(bounds_of(cx, "content"), viewport, "content panel");
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
async fn keyboard_shortcuts_move_between_destinations(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    focus_root(&view, cx);

    let transfers = Destination::Transfers.keystroke(Platform::current());
    cx.simulate_keystrokes(&transfers);
    cx.run_until_parked();
    assert_eq!(
        destination_of(&view, cx),
        Destination::Transfers,
        "the {transfers} shortcut did not reach the shell"
    );

    cx.dispatch_action(GoToSessions);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Sessions);

    cx.dispatch_action(GoToHosts);
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Hosts);
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
async fn a_planned_capability_shows_a_notice_instead_of_starting_work(cx: &mut TestAppContext) {
    // Transfers are still unimplemented, so their primary action must say so
    // instead of pretending a transfer was started.
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    focus_root(&view, cx);
    cx.dispatch_action(GoToTransfers);
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("notice").is_none(),
        "no notice before an action"
    );
    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(
            workspace.notice(),
            Some(crate::app::workspace::Notice::UnavailableBuild)
        );
        assert_eq!(
            workspace.state(Destination::Transfers),
            SectionState::Planned
        );
        assert_eq!(workspace.item_count(Destination::Transfers), 0);
        assert_eq!(workspace.item_count(Destination::Hosts), 0);
        assert!(
            view.read(app).pane().is_none(),
            "an unimplemented action must not start a terminal"
        );
    });
    assert!(
        cx.debug_bounds("notice").is_some(),
        "the notice explaining that nothing started must be visible"
    );
}

#[gpui::test]
async fn every_empty_loading_and_failed_state_renders_a_state_card(cx: &mut TestAppContext) {
    // The hosts destination is implemented, so the only non-populated state it can
    // really be in is "empty". The other three are exercised on a destination that
    // still has them, through the same state card.
    let cases = [
        (
            "planned",
            Destination::Transfers,
            Workspace::with_sections(
                Section::<Session>::planned(),
                Section::<HostConfig>::planned(),
                Section::<Transfer>::planned(),
            ),
            SectionState::Planned,
        ),
        (
            "loading",
            Destination::Transfers,
            Workspace::with_sections(
                Section::<Session>::planned(),
                Section::<HostConfig>::planned(),
                Section::<Transfer>::loading(),
            ),
            SectionState::Loading,
        ),
        (
            "failed",
            Destination::Transfers,
            Workspace::with_sections(
                Section::<Session>::planned(),
                Section::<HostConfig>::planned(),
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
                Section::<Session>::planned(),
                Section::<HostConfig>::ready(Vec::new()),
                Section::<Transfer>::planned(),
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

        let viewport = cx.update(|window, _app| window.viewport_size());
        assert_eq!(
            cx.update(|_window, app| view.read(app).workspace().state(destination)),
            expected,
            "the {name} fixture did not keep its state"
        );
        assert_fits_inside(bounds_of(cx, "state.card"), viewport, name);
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

#[gpui::test]
async fn a_populated_section_renders_one_row_per_real_object(cx: &mut TestAppContext) {
    let workspace = Workspace::with_sections(
        Section::ready(vec![
            connected_local_session("pwsh"),
            connected_local_session("cmd"),
        ]),
        Section::<HostConfig>::planned(),
        Section::<Transfer>::planned(),
    );
    let (view, cx) =
        cx.add_window_view(move |_window, cx| ShelluneView::with_workspace(cx, workspace));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    let viewport = cx.update(|window, _app| window.viewport_size());
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().state(Destination::LocalShells)),
        SectionState::Populated
    );
    assert_fits_inside(bounds_of(cx, "row.0"), viewport, "first session row");
    assert_fits_inside(bounds_of(cx, "row.1"), viewport, "second session row");
    assert!(
        cx.debug_bounds("row.2").is_none(),
        "two sessions must not render three rows"
    );
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[test]
fn only_a_ready_section_with_items_can_look_successful() {
    let states = [
        (Section::<u8>::planned(), SectionState::Planned),
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
        Section::<HostConfig>::planned(),
        Section::<Transfer>::planned(),
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
    // Transfers are still unimplemented: their primary action must report that
    // nothing started rather than pretend a transfer was queued.
    workspace.select(Destination::Transfers);
    assert_eq!(workspace.request_primary(), PrimaryOutcome::Unavailable);
    assert_eq!(
        workspace.notice(),
        Some(crate::app::workspace::Notice::UnavailableBuild)
    );
    assert_eq!(workspace.item_count(Destination::Transfers), 0);
    assert_eq!(
        workspace.state(Destination::Transfers),
        SectionState::Planned
    );

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
fn text_size_steps_through_the_three_levels_and_wraps() {
    assert_eq!(Zoom::default(), Zoom::Regular);
    assert_eq!(Zoom::Compact.next(), Zoom::Regular);
    assert_eq!(Zoom::Regular.next(), Zoom::Comfortable);
    assert_eq!(Zoom::Comfortable.next(), Zoom::Compact);
    assert!(Zoom::Compact.factor() < Zoom::Regular.factor());
    assert!(Zoom::Regular.factor() < Zoom::Comfortable.factor());

    let metrics = Metrics::new(Zoom::Comfortable);
    assert!(metrics.body_size() > Metrics::new(Zoom::Compact).body_size());
    assert!(metrics.rail_width() > Metrics::new(Zoom::Compact).rail_width());
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
            workspace,
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
            state_planned,
            planned_title,
            planned_local,
            planned_hosts,
            planned_sessions,
            planned_transfers,
            state_empty,
            state_ready,
            state_loading,
            loading_title,
            loading_body,
            state_failed,
            failed_title,
            recovery_label,
            recovery_retry,
            recovery_create_host,
            recovery_import_config,
            recovery_choose_shell,
            next_step,
            primary_local,
            primary_hosts,
            primary_sessions,
            primary_transfers,
            unavailable_notice,
            object_label,
            hint_nav_action,
            zoom_control,
            language_control,
            count_hosts,
            count_sessions,
            count_transfers,
            local_shell_fallback,
            nothing_running,
            shell_picker_label,
            shell_custom_button,
            shell_custom_placeholder,
            shell_none_found,
            terminal_live,
            terminal_failed,
            terminal_exited,
            terminal_terminated,
            terminal_copy,
            hosts_search_placeholder,
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
    assert_eq!(
        Destination::LocalShells.shortcut_label(Platform::Windows),
        "Ctrl+1"
    );
    assert_eq!(
        Destination::Transfers.shortcut_label(Platform::Windows),
        "Ctrl+4"
    );
    assert_eq!(
        Destination::Transfers.shortcut_label(Platform::Linux),
        "Ctrl+4"
    );
    assert_eq!(
        Destination::LocalShells.shortcut_label(Platform::MacOs),
        "⌘1"
    );
    assert_eq!(Destination::Transfers.shortcut_label(Platform::MacOs), "⌘4");

    assert_eq!(Destination::Hosts.keystroke(Platform::Windows), "ctrl-2");
    assert_eq!(Destination::Hosts.keystroke(Platform::MacOs), "cmd-2");

    assert_eq!(Destination::LocalShells.next(), Destination::Hosts);
    assert_eq!(Destination::Transfers.next(), Destination::LocalShells);
    assert_eq!(Destination::LocalShells.previous(), Destination::Transfers);
    for destination in Destination::ALL {
        assert_eq!(destination.next().previous(), destination);
        assert_eq!(Destination::ALL[destination.index()], destination);
    }
}

#[test]
fn the_navigation_hint_names_the_keys_the_keymap_binds() {
    for platform in [Platform::Windows, Platform::MacOs, Platform::Linux] {
        let hint = copy::nav_hint(&ENGLISH, platform);
        // The hint joins the labels the rail actually shows for the first and last
        // entries, so it cannot name a key this platform does not bind.
        let first = Destination::LocalShells.shortcut_label(platform);
        let last_digit = Destination::Transfers
            .shortcut_label(platform)
            .chars()
            .last()
            .expect("a shortcut label ends with a digit");
        assert!(
            hint.starts_with(&first),
            "{platform:?} hint {hint:?} does not start with {first:?}"
        );
        // The printed range must end on the digit bound for the last entry.
        let range = hint
            .split_once(' ')
            .map_or(hint.as_str(), |(range, _)| range);
        assert!(
            range.ends_with(last_digit),
            "{platform:?} hint {hint:?} does not end with {last_digit:?}"
        );
        assert_eq!(
            hint.contains("Ctrl"),
            platform != Platform::MacOs,
            "{platform:?} hint {hint:?} names a modifier the keymap does not bind"
        );
        assert!(
            hint.contains(platform.shortcut_modifier()),
            "{platform:?} hint {hint:?} omits the platform modifier"
        );
    }

    // The Chinese table carries the same platform-derived range, not a
    // Windows-only one.
    let english = copy::nav_hint(&ENGLISH, Platform::MacOs);
    let chinese = copy::nav_hint(&CHINESE, Platform::MacOs);
    let range_of = |hint: &str| hint.split_once(' ').map(|(range, _)| range.to_owned());
    assert_eq!(
        range_of(&chinese),
        range_of(&english),
        "the Chinese hint must use the same range as the English one"
    );
    let hint = chinese;
    assert!(
        hint.contains("\u{5207}\u{6362}"),
        "the Chinese hint lost its meaning: {hint}"
    );
}

#[test]
fn a_section_count_uses_the_unit_of_its_own_destination() {
    assert_eq!(Destination::Hosts.count_unit(&ENGLISH), "host");
    assert_eq!(Destination::Transfers.count_unit(&ENGLISH), "transfer");
    assert_eq!(Destination::LocalShells.count_unit(&ENGLISH), "session");
    assert_eq!(Destination::Sessions.count_unit(&ENGLISH), "session");
    for destination in Destination::ALL {
        assert_ne!(
            destination.count_unit(&ENGLISH),
            destination.label(&ENGLISH),
            "{destination:?} uses its own name as a count unit"
        );
    }
}

#[test]
fn a_status_chip_never_stands_in_for_a_count_or_a_unit() {
    let units = [
        ENGLISH.count_hosts,
        ENGLISH.count_sessions,
        ENGLISH.count_transfers,
    ];
    let states = [
        SectionState::Planned,
        SectionState::Loading,
        SectionState::Empty,
        SectionState::Failed,
        SectionState::Populated,
    ];
    for state in states {
        let label = state.chip_label(&ENGLISH);
        assert!(
            !units.contains(&label),
            "{state:?} uses the unit word {label:?} as its status chip"
        );
        assert!(
            !label.chars().any(|c| c.is_ascii_digit()),
            "{state:?} chip carries a count: {label:?}"
        );
    }
    assert_ne!(
        SectionState::Populated.chip_label(&ENGLISH),
        SectionState::Empty.chip_label(&ENGLISH),
        "a section with real data must not read like an empty one"
    );
    assert_eq!(SectionState::Populated.chip_label(&ENGLISH), "Ready");
}

#[test]
fn key_bindings_cover_every_destination_plus_the_shell_controls() {
    let bindings = key_bindings(Platform::current());
    assert_eq!(
        bindings.len(),
        Destination::ALL.len() + 5,
        "four destinations, next and previous, zoom, language and dismiss"
    );
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

impl Render for PaneHarness {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        div().size_full().child(self.pane.clone())
    }
}

fn pane_with<'a>(
    content: &str,
    cx: &'a mut TestAppContext,
) -> (Entity<TerminalPane>, &'a mut VisualTestContext) {
    let pane = cx.new(|pane_cx| {
        TerminalPane::without_process(
            LocalShell::new("Test shell", "test-shell"),
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
        cx.debug_bounds("state.card").is_some(),
        "the failure must be shown in the section card"
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
    // The field emptied itself, so reopening it cannot offer the same path again.
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

/// Clicks the centre of a rendered element.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = bounds_of(cx, selector);
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
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
        cx.debug_bounds("state.card").is_some(),
        "an empty store explains itself"
    );

    click(cx, "header.primary");
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
async fn an_invalid_port_is_refused_and_nothing_is_written(cx: &mut TestAppContext) {
    let dir = TempDir::new("port");
    let (view, cx) = hosts_view(cx, dir.store());

    click(cx, "header.primary");
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

    click(cx, "header.primary");
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
    // result rather than a planned capability.
    click(cx, "hosts.delete.0");
    click(cx, "hosts.confirm.delete");
    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(workspace.state(Destination::Hosts), SectionState::Empty);
        assert_eq!(workspace.item_count(Destination::Hosts), 0);
    });
    assert!(
        cx.debug_bounds("state.card").is_some(),
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

    click(cx, "header.primary");
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
async fn the_import_panel_opens_on_the_platform_file_and_reads_a_named_one(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("import-open");
    let (view, cx) = hosts_view(cx, dir.store());

    click(cx, "hosts.import");
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
        assert!(!message.failed());
        assert!(message.text().contains('2'), "{}", message.text());
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
