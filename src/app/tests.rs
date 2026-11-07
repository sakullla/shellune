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
    Endpoint, EventTag, ForwardBook, ForwardRule, HostConfig, LocalShell, PaneNode, PaneTarget,
    PanelConfig, PanelSection, Session, SessionId, SessionKind, SessionState, SplitAxis,
    TerminalMode, Transfer, TransferDirection, WorkspaceSnapshot, WorkspaceTab,
};
use shellune_storage::{
    ForwardStore, HostBook, HostEdit, HostKey, HostStore, TrustStore, WORKSPACE_FILE_NAME,
    WorkspaceStore,
};
use shellune_terminal::{BlockStatus, GridSize, TermMode, encode_paste};

use crate::app::actions::{CycleTextSize, GoToHosts, GoToSessions, GoToTransfers, ToggleLanguage};
use crate::app::copy::{CHINESE, ENGLISH, Locale, Strings};
use crate::app::hosts::HostView;
use crate::app::layout::Direction;
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
    // The first page is the welcome: the shells it offers are the current object.
    assert_fits_inside(bounds_of(cx, "shell.picker"), viewport, "shell picker");
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
async fn the_transfers_action_leads_to_the_hosts_when_nothing_can_browse(cx: &mut TestAppContext) {
    // The transfers page has no transfer of its own to start - a transfer is started
    // from the file view of a session - so with no connection that can browse, its
    // header action takes the user to the hosts instead of pretending one was queued.
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    focus_root(&view, cx);
    cx.dispatch_action(GoToTransfers);
    cx.run_until_parked();

    let bounds = bounds_of(cx, "header.primary");
    let center = bounds.center();
    cx.simulate_click(point(center.x, center.y), Modifiers::default());
    cx.run_until_parked();

    cx.update(|_window, app| {
        let workspace = view.read(app).workspace();
        assert_eq!(
            workspace.destination(),
            Destination::Hosts,
            "the action leads to where a connection is made"
        );
        assert_eq!(
            workspace.notice(),
            None,
            "nothing failed, so nothing is noticed"
        );
        assert_eq!(workspace.state(Destination::Transfers), SectionState::Empty);
        assert_eq!(workspace.item_count(Destination::Transfers), 0);
        assert_eq!(workspace.item_count(Destination::Hosts), 0);
        assert!(
            view.read(app).pane().is_none(),
            "leading to the hosts must not start a terminal"
        );
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
        Section::<HostConfig>::ready(Vec::new()),
        Section::<Transfer>::ready(Vec::new()),
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
            nav_hint_short,
            shell_default,
            shell_custom_hint,
            welcome_hosts,
            zoom_control,
            language_control,
            count_hosts,
            count_sessions,
            count_transfers,
            local_shell_fallback,
            idle,
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
            palette_open,
            palette_hint_short,
            palette_placeholder,
            palette_empty,
            palette_group_sessions,
            palette_group_shells,
            palette_group_hosts,
            palette_group_navigate,
            palette_group_actions,
            palette_close_tab,
            palette_language,
            palette_text_size,
            palette_current,
            palette_key_move,
            palette_key_run,
            palette_key_close,
            tab_menu_duplicate,
            tab_menu_close,
            tab_menu_close_others,
            tab_menu_close_right,
            theme_control,
            theme_system,
            theme_light,
            theme_dark,
            palette_theme,
            quick_connect,
            quick_connect_hint,
            quick_connect_title,
            quick_save_host,
            quick_saved,
            quick_unsaved_note,
            attempt_others,
            attempt_cancel,
            palette_group_recent,
            workspace_current,
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
            sidebar_collapse,
            sidebar_expand,
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
            pane_remove,
            pane_close,
            pane_resize_less,
            pane_resize_more,
            pane_move_left,
            pane_move_right,
            pane_move_up,
            pane_move_down,
            tab_menu_move_left,
            tab_menu_move_right,
            workspace_empty_title,
            workspace_empty_body,
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
            terminal_search_next,
            terminal_search_close,
            terminal_paste_title,
            terminal_paste_target,
            terminal_paste_confirm,
            terminal_paste_cancel,
            terminal_paste_lines,
            terminal_no_blocks,
            terminal_blocks_failed,
            terminal_filter_all,
            terminal_filter_running,
            terminal_filter_unknown,
            terminal_filter_failed,
            terminal_filter_zero,
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
        let hint = format!("{} {}", platform.shortcut_range(), ENGLISH.nav_hint_short);
        // The welcome page shows this range beside its label; it joins the shortcut
        // labels of the first and last entries, so it cannot name a key this platform
        // does not bind.
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
    let english = format!(
        "{} {}",
        Platform::MacOs.shortcut_range(),
        ENGLISH.nav_hint_short
    );
    let chinese = format!(
        "{} {}",
        Platform::MacOs.shortcut_range(),
        CHINESE.nav_hint_short
    );
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
        Destination::ALL.len() + 6 + 5 + crate::app::TAB_JUMP_COUNT + 2 + 1 + 4 + 4 + 4 + 2,
        "four destinations, next and previous, zoom, new shell, language and dismiss; \
         the palette, the sidebar, close tab, next and previous tab; one jump per \
         tab digit; split right and split down, zoom pane, focus, resize and move in \
         four directions, and move tab left and right"
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
    assert_eq!(
        cx.update(|_window, app| pane.read(app).pending_paste_text().map(str::to_owned)),
        Some(paste.to_owned())
    );
    assert!(cx.debug_bounds("terminal.paste").is_some());
    assert!(cx.debug_bounds("terminal.paste.target").is_some());

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
    click(cx, "terminal.paste.cancel");
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

    click(cx, "terminal.paste.confirm");
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
    click(cx, "terminal.paste.confirm");
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

    // With a terminal on screen the shell choices live in the header's menu.
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
    // The open shell menu covers the window, so the first press only dismisses it.
    click(cx, "nav.hosts");
    click(cx, "nav.hosts");
    assert_eq!(destination_of(&view, cx), Destination::Hosts);
    click(cx, "nav.local");
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    assert_eq!(
        cx.update(|_window, app| view.read(app).active_session()),
        Some(open)
    );
    click(cx, "palette.open");
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
        cx.debug_bounds("shell.menu").is_none(),
        "the empty page shows the shells as tiles, not behind a menu"
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

    // The control in the status bar is the in-terminal path to the same intent: it has
    // to keep working, or the shortcut above would have made the language unreachable.
    let control = bounds_of(cx, "status.language");
    let control_center = control.center();
    cx.simulate_click(
        point(control_center.x, control_center.y),
        Modifiers::default(),
    );
    cx.run_until_parked();
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
    // result, not a missing capability.
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

    click(cx, "palette.open");
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
    click(cx, "palette.open");
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

/// The ids of the records that are closed, for the management tests.
fn closed_records(cx: &mut VisualTestContext, view: &Entity<ShelluneView>) -> Vec<SessionId> {
    cx.update(|_window, app| view.read(app).workspace().closed_sessions())
}

#[gpui::test]
async fn the_sessions_page_lists_every_session_while_tabs_are_open(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    // One tab closed leaves a record; one is still open and on screen.
    click(cx, "session.tab.close.0");
    assert_eq!(closed_records(cx, &view).len(), 1);

    click(cx, "nav.sessions");
    assert_eq!(destination_of(&view, cx), Destination::Sessions);
    assert!(
        cx.debug_bounds("row.0").is_some() && cx.debug_bounds("row.1").is_some(),
        "the page reached from the sidebar lists the sessions, the closed one included"
    );
    assert!(
        cx.debug_bounds("sessions.closed").is_some(),
        "the list says how many records are closed and offers to clear them"
    );
}

#[gpui::test]
async fn a_closed_record_can_be_removed_and_all_closed_ones_cleared(cx: &mut TestAppContext) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    click(cx, "session.tab.close.1");
    click(cx, "session.tab.close.0");
    assert_eq!(closed_records(cx, &view).len(), 2);

    // Every tab is closed, so the local page lists the records.
    click(cx, "nav.local");
    click(cx, "row.remove.0");
    let remaining = closed_records(cx, &view);
    assert_eq!(
        remaining.len(),
        1,
        "only the record whose button was pressed went"
    );
    assert!(
        cx.update(|_window, app| view.read(app).workspace().session(tabs[0]).is_none()),
        "the first record is the one removed"
    );

    click(cx, "sessions.remove_closed");
    assert!(closed_records(cx, &view).is_empty());
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().sessions().count()),
        0,
        "nothing but closed records was there, so nothing is left"
    );
}

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

#[gpui::test]
async fn a_session_row_menu_opens_on_right_click_and_reopens_a_closed_shell(
    cx: &mut TestAppContext,
) {
    let Some((view, cx)) = two_shells(cx) else {
        return;
    };
    click(cx, "session.tab.close.1");
    click(cx, "session.tab.close.0");
    click(cx, "nav.local");
    let closed = closed_records(cx, &view);

    let center = bounds_of(cx, "row.0").center();
    cx.simulate_mouse_down(center, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(center, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.update(|_window, app| view.read(app).session_menu_open()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        !cx.update(|_window, app| view.read(app).session_menu_open()),
        "Escape closes the row menu"
    );

    cx.simulate_mouse_down(center, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(center, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    click(cx, "session.menu.reopen");
    let tabs = cx.update(|_window, app| view.read(app).open_tabs());
    assert_eq!(tabs.len(), 1, "reopening started the shell again in a tab");
    let after = closed_records(cx, &view);
    assert_eq!(
        after.len(),
        closed.len() - 1,
        "the reopened record does not stay behind as a closed copy"
    );
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
    click(cx, "header.primary");
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
    click(cx, "header.primary");
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
    assert!(
        cx.debug_bounds("session.identity").is_some(),
        "the connected session names itself"
    );
}

// ---------------------------------------------------------------------------
// One workspace for local and remote sessions (R1, R2, R3, R12)
// ---------------------------------------------------------------------------

#[gpui::test]
async fn local_and_remote_sessions_share_the_workspace(cx: &mut TestAppContext) {
    let (view, cx, _dir) =
        dialog_view(cx, vec![stored_host("web", "10.0.0.5", 22, "deploy", None)]);
    click(cx, "nav.local");
    let local = start_local_session(cx, &view);

    click(cx, "nav.hosts");
    assert_eq!(
        cx.update(|_window, app| view.read(app).workspace().context()),
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
        cx.update(|_window, app| view.read(app).workspace().context()),
        shellune_core::PanelSection::None,
        "the connected session comes back to the workspace"
    );
    assert!(cx.debug_bounds("session.bar").is_some());
    assert!(cx.debug_bounds("session.identity").is_some());
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

    click(cx, "nav.sessions");
    assert_eq!(destination_of(&view, cx), Destination::Sessions);
    assert!(
        cx.debug_bounds("row.0").is_some(),
        "the sessions page lists what the workspace is showing"
    );
    assert!(
        cx.debug_bounds("session.identity").is_some(),
        "leaving the terminal does not hide which session is current"
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

    click(cx, "nav.sessions");
    assert_eq!(destination_of(&view, cx), Destination::Sessions);
    click(cx, "palette.open");
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
    assert!(cx.debug_bounds("session.identity").is_some());
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
async fn quick_connect_opens_on_the_sessions_list(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.simulate_resize(design_window());
    cx.run_until_parked();

    click(cx, "nav.sessions");
    assert_eq!(destination_of(&view, cx), Destination::Sessions);
    assert!(
        cx.debug_bounds("quick.panel").is_none(),
        "the sessions list does not show the form until it is asked for"
    );

    click(cx, "shell.menu");
    click(cx, "menu.quick");
    assert_eq!(
        destination_of(&view, cx),
        Destination::Sessions,
        "opening quick connect stays on the sessions list"
    );
    assert!(cx.debug_bounds("quick.panel").is_some());
    assert!(cx.debug_bounds("quick.address").is_some());
    assert!(cx.update(|_window, app| view.read(app).quick_form_open()));

    click(cx, "quick.close");
    assert!(!cx.update(|_window, app| view.read(app).quick_form_open()));

    click(cx, "palette.open");
    cx.simulate_input("Quick connect");
    cx.run_until_parked();
    let titles = cx.update(|_window, app| view.read(app).palette_titles());
    assert_eq!(
        titles.first().map(String::as_str),
        Some(ENGLISH.quick_connect),
        "the palette offers quick connect, got {titles:?}"
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(destination_of(&view, cx), Destination::Sessions);
    assert!(cx.update(|_window, app| view.read(app).quick_form_open()));
    assert!(
        cx.debug_bounds("quick.panel").is_some(),
        "the command palette mounts the form on the sessions list"
    );
    assert!(cx.debug_bounds("quick.address").is_some());
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

    click(cx, "status.language");
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

    click(cx, "status.language");
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

    click(cx, "status.language");
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
    click(cx, "status.language");
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
    click(cx, "hosts.connect.1");
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

    click(cx, "session.tab.close.0");
    assert_eq!(
        cx.update(|_window, app| view.read(app).pending_attempt_count()),
        1,
        "closing one attempt leaves the other waiting"
    );
    let waiting = cx.update(|_window, app| view.update(app, |view, cx| view.pump_remote_once(cx)));
    assert!(waiting, "the remaining attempt is still polled");

    click(cx, "nav.local");
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    click(cx, "palette.open");
    assert!(cx.update(|_window, app| view.read(app).palette_open()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    click(cx, "nav.hosts");
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
    click(cx, "nav.sessions");
    assert_eq!(destination_of(&view, cx), Destination::Sessions);
    click(cx, "palette.open");
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

    click(cx, "nav.local");
    assert_eq!(destination_of(&view, cx), Destination::LocalShells);
    if let Some(local) = local {
        assert_eq!(
            session_state(cx, &view, local),
            Some(SessionState::Connected)
        );
        let sent = sent_after_typing(cx, &view, local, "still-typing");
        assert!(sent.contains("still-typing"), "{sent:?}");
    }
    click(cx, "palette.open");
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

#[gpui::test]
async fn the_sidebar_collapses_to_its_icons_and_navigation_keeps_working(cx: &mut TestAppContext) {
    let (view, cx) = cx.add_window_view(|_window, cx| ShelluneView::new(cx));
    cx.update(|_window, app| app.bind_keys(key_bindings(Platform::current())));
    cx.simulate_resize(minimum_window());
    cx.run_until_parked();
    let wide = bounds_of(cx, "rail").size.width;

    click(cx, "rail.toggle");
    assert!(cx.update(|_window, app| view.read(app).rail_collapsed()));
    // The rail is drawn again at its narrow width (bounds of a selector keep the latest
    // frame that drew it, and both widths draw "rail").
    let narrow = bounds_of(cx, "rail").size.width;
    assert!(
        narrow < wide / 2.0,
        "collapsed to its icons: {narrow:?} vs {wide:?}"
    );

    // Every page is still one click away, by its icon.
    click(cx, "nav.hosts");
    assert_eq!(destination_of(&view, cx), Destination::Hosts);
    let viewport = minimum_window();
    for destination in Destination::ALL {
        assert_fits_inside(
            bounds_of(cx, destination.debug_selector()),
            viewport,
            destination.debug_selector(),
        );
    }

    // The shortcut expands it again from anywhere in the window.
    focus_root(&view, cx);
    cx.simulate_keystrokes(Platform::current().sidebar_keystroke());
    cx.run_until_parked();
    assert!(!cx.update(|_window, app| view.read(app).rail_collapsed()));
}

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
    for token in tokens.into_iter().chain(
        palette::AVATARS
            .iter()
            .flat_map(|(tint, text)| [*tint, *text]),
    ) {
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
    click(cx, "theme.control");
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
    click(cx, "theme.control");
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
        cx.debug_bounds("workspace.empty").is_some(),
        "the last pane leaves an empty state"
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
