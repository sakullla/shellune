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
    EventTag, HostConfig, LocalShell, Session, SessionId, SessionKind, SessionState, Transfer,
    TransferDirection,
};
use shellune_storage::{HostBook, HostEdit, HostKey, HostStore, TrustStore};
use shellune_terminal::GridSize;

use crate::app::actions::{CycleTextSize, GoToHosts, GoToSessions, GoToTransfers, ToggleLanguage};
use crate::app::copy::{self, CHINESE, ENGLISH, Locale, Strings};
use crate::app::hosts::HostView;
use crate::app::nav::{Destination, Platform};
use crate::app::product;
use crate::app::remote::{MethodChoice, RemotePanel, RemoteStatus};
use crate::app::terminal::{PaneState, SourceEvent, TerminalPane};
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
    // The transfers page has no primary action of its own - a transfer is started from
    // the file view of a session - so its header action must say that nothing started
    // instead of pretending one was queued.
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
        assert_eq!(workspace.state(Destination::Transfers), SectionState::Empty);
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
            session_kind_local,
            session_kind_remote,
            session_close,
            session_reconnect,
            session_reconnect_no_shell,
            session_reconnect_missing_host,
            files_open,
            files_close,
            files_host_label,
            files_path_label,
            files_up,
            files_refresh,
            files_parent_row,
            files_loading,
            files_empty,
            files_unknown_path,
            files_failed,
            files_terminal_note,
            files_kind_directory,
            files_kind_file,
            files_kind_link,
            files_kind_other,
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

    cx.simulate_input("ping -n 60 127.0.0.1");
    cx.simulate_keystrokes("enter");
    // The program has to be really running before the key is pressed, or the press proves
    // nothing: a ping prints one reply per second and `TTL=` is the part of that line no
    // locale changes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut replies = 0;
    while std::time::Instant::now() < deadline {
        cx.update(|_window, app| {
            pane.update(app, |pane, cx| pane.pump_once(cx));
        });
        screen = cx.update(|_window, app| pane.read(app).visible_text().join("\n"));
        replies = screen.matches("TTL=").count();
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

    // Closing the panel ends the session and leaves no terminal behind.
    click(cx, "remote.close");
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
        cx.debug_bounds("session.bar").is_some(),
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
    click(cx, "nav.sessions");
    assert_eq!(
        destination_of(&view, cx),
        Destination::Sessions,
        "a pending connection does not pin the navigation"
    );
    click(cx, "nav.local");
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
    assert_eq!(destination_of(&view, cx), Destination::Sessions);
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
    click(cx, "nav.sessions");

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
    assert!(
        cx.debug_bounds("session.reconnect").is_none(),
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
    // A remote terminal is only drawn on the sessions destination.
    click(cx, "nav.sessions");
    id
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
    click(cx, "nav.sessions");
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
    assert!(
        cx.debug_bounds("files.report").is_some(),
        "what the server did is stated"
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
        cx.debug_bounds("files.report").is_some(),
        "the report of what the server did is still there"
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
        cx.debug_bounds("transfers.panel").is_some(),
        "the transfer list belongs to the file view"
    );
    assert!(
        cx.debug_bounds("transfers.empty").is_some(),
        "nothing was transferred yet, and that is said instead of implied"
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
        update: shellune_core::TransferUpdate::Cancelled { residue: None },
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
    click(cx, "nav.transfers");
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
