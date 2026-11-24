//! User-facing copy, in English and Chinese (R12, R18).
//!
//! Every string the shell renders comes from here so that both languages are
//! complete at the same time and no view invents wording inline. The Chinese
//! column is not a literal translation of the English one: it is written for a
//! Chinese reader, and the tests only require that both are present, non-empty
//! and renderable.
//!
//! The product name, pronunciation and the two taglines stay in
//! [`crate::app::product`] because R18 fixes them independently of the UI
//! language.

use shellune_core::{SessionState, SnippetProblem, TabAccent, TransferState};

/// UI language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    /// English (default).
    #[default]
    English,
    /// Simplified Chinese.
    Chinese,
}

impl Locale {
    /// Both languages, in display order.
    pub const ALL: [Self; 2] = [Self::English, Self::Chinese];

    /// The string table for this language.
    pub fn strings(self) -> &'static Strings {
        match self {
            Self::English => &ENGLISH,
            Self::Chinese => &CHINESE,
        }
    }

    /// Name of the language in that language, for the toggle.
    pub fn endonym(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "中文",
        }
    }

    /// The language the window is drawn in right now.
    ///
    /// Set by the window each time it draws; small views that are not handed the
    /// workspace, such as a text field's context menu, read it here.
    pub fn current() -> Self {
        CURRENT_LOCALE.with(std::cell::Cell::get)
    }

    /// Records the language the window is being drawn in; see [`Locale::current`].
    pub fn set_current(self) {
        CURRENT_LOCALE.with(|current| current.set(self));
    }

    /// The other language.
    pub fn toggle(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|locale| *locale == self)
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

thread_local! {
    /// The language the window is drawn in; per thread, because a test runs its window
    /// on its own thread and must not see another test's choice.
    static CURRENT_LOCALE: std::cell::Cell<Locale> = const { std::cell::Cell::new(Locale::English) };
}

/// One column of UI copy.
///
/// The struct is large but flat on purpose: a missing field is a compile error in
/// both languages at once, which is cheaper than a runtime fallback table.
#[derive(Debug)]
pub struct Strings {
    /// Navigation entry: local shells.
    pub nav_local: &'static str,
    /// One-line description of the local shells destination.
    pub nav_local_summary: &'static str,
    /// Navigation entry: saved hosts.
    pub nav_hosts: &'static str,
    /// One-line description of the hosts destination.
    pub nav_hosts_summary: &'static str,
    /// Navigation entry: open sessions.
    pub nav_sessions: &'static str,
    /// One-line description of the sessions destination.
    pub nav_sessions_summary: &'static str,
    /// Navigation entry: file transfers.
    pub nav_transfers: &'static str,
    /// One-line description of the transfers destination.
    pub nav_transfers_summary: &'static str,
    /// Empty-state title on the local shells destination.
    pub empty_local_title: &'static str,
    /// Empty-state body on the local shells destination.
    pub empty_local_body: &'static str,
    /// Empty-state title on the hosts destination.
    pub empty_hosts_title: &'static str,
    /// Empty-state body on the hosts destination.
    pub empty_hosts_body: &'static str,
    /// Empty-state title on the sessions destination.
    pub empty_sessions_title: &'static str,
    /// Empty-state body on the sessions destination.
    pub empty_sessions_body: &'static str,
    /// Empty-state title on the transfers destination.
    pub empty_transfers_title: &'static str,
    /// Empty-state body on the transfers destination.
    pub empty_transfers_body: &'static str,
    /// Title shown while a section is loading.
    pub loading_title: &'static str,
    /// Body shown while a section is loading.
    pub loading_body: &'static str,
    /// Title of a failed section.
    pub failed_title: &'static str,
    /// Label introducing the suggested recovery step.
    pub recovery_label: &'static str,
    /// Recovery action: retry.
    pub recovery_retry: &'static str,
    /// Recovery action: create a host.
    pub recovery_create_host: &'static str,
    /// Recovery action: import an SSH configuration.
    pub recovery_import_config: &'static str,
    /// Recovery action: pick a shell.
    pub recovery_choose_shell: &'static str,
    /// Label introducing the next step of an empty section.
    pub next_step: &'static str,
    /// Primary action on the hosts destination.
    pub primary_hosts: &'static str,
    /// Feedback after an unavailable action was requested.
    pub unavailable_notice: &'static str,
    /// Suffix for a count of hosts.
    pub count_hosts: &'static str,
    /// Suffix for a count of exactly one host.
    pub count_host_one: &'static str,
    /// Fallback name for a local session without a chosen shell.
    pub local_shell_fallback: &'static str,
    /// Button that reveals the free-text shell path field.
    pub shell_custom_button: &'static str,
    /// Placeholder in the free-text shell path field.
    pub shell_custom_placeholder: &'static str,
    /// Shown when no shell was found on this machine.
    pub shell_none_found: &'static str,
    /// Terminal state: the shell is running.
    pub terminal_live: &'static str,
    /// Terminal state: the shell could not be started.
    pub terminal_failed: &'static str,
    /// Terminal state: the shell exited on its own.
    pub terminal_exited: &'static str,
    /// Terminal state: the shell was killed by a signal.
    pub terminal_terminated: &'static str,
    /// Terminal state: the session was live and stopped answering.
    ///
    /// Kept apart from [Self::terminal_failed] because the advice differs: a session
    /// that ran and was lost is worth reconnecting, while a program that never started
    /// needs its path fixed (R17).
    pub terminal_disconnected: &'static str,
    /// Button that opens the raw diagnostic of a session that ended.
    pub session_details_show: &'static str,
    /// Button that closes that diagnostic again.
    pub session_details_hide: &'static str,
    /// Terminal note: the login shell could not be asked for its environment (R14).
    pub terminal_environment_unavailable: &'static str,
    /// Terminal note: the login shell answered with something that is no environment.
    pub terminal_environment_unreadable: &'static str,
    /// Terminal note: the locale was not UTF-8 and was replaced (R15).
    pub terminal_environment_locale_replaced: &'static str,
    /// Terminal note: no locale was inherited, so a UTF-8 one was added (R15).
    pub terminal_environment_locale_added: &'static str,
    /// Button that copies the terminal selection.
    pub terminal_copy: &'static str,
    /// Tooltip of the button that makes the terminal text smaller.
    pub terminal_smaller: &'static str,
    /// Tooltip of the button that makes the terminal text larger.
    pub terminal_larger: &'static str,
    /// The second line of the custom-path launcher tile.
    pub shell_custom_hint: &'static str,
    /// Placeholder in the settings page's action search.
    pub keys_search_placeholder: &'static str,
    /// Host list view: every saved host.
    pub hosts_view_all: &'static str,
    /// Host list view: hosts that were really connected, newest first.
    pub hosts_view_recent: &'static str,
    /// Shown when no connection has been recorded yet.
    pub hosts_recent_empty: &'static str,
    /// Shown when a search matches no saved host.
    pub hosts_no_match: &'static str,
    /// Form label: host name.
    pub host_field_name: &'static str,
    /// Form label: host address.
    pub host_field_address: &'static str,
    /// Form label: TCP port.
    pub host_field_port: &'static str,
    /// Form label: login user.
    pub host_field_user: &'static str,
    /// Form label: group label.
    pub host_field_group: &'static str,
    /// Form label: the private key file a connection may use.
    pub host_field_identity: &'static str,
    /// Note that only the key path is stored, never the key.
    pub host_identity_note: &'static str,
    /// Title of the editor when a new host is being created.
    pub host_editor_new: &'static str,
    /// Title of the editor when a saved host is being changed.
    pub host_editor_edit: &'static str,
    /// Button that stores the form.
    pub host_save: &'static str,
    /// Button that closes the form without storing anything.
    pub host_cancel: &'static str,
    /// Button that opens a saved host in the editor.
    pub host_edit: &'static str,
    /// Button that starts deleting a saved host.
    pub host_delete: &'static str,
    /// Question shown before a host is deleted.
    pub host_delete_question: &'static str,
    /// Button that confirms the deletion.
    pub host_delete_confirm: &'static str,
    /// Note explaining what deleting a host does and does not touch.
    pub host_delete_note: &'static str,
    /// Feedback after a host was stored.
    pub hosts_saved: &'static str,
    /// Feedback after a host was removed.
    pub hosts_deleted: &'static str,
    /// Button that reads the host file again after a failure.
    pub hosts_retry: &'static str,
    /// Label introducing when a host was last reached.
    pub host_last_connected: &'static str,
    /// Unit after a count of successful connections.
    pub host_connection_count: &'static str,

    /// Button that opens the SSH configuration import panel.
    pub import_open: &'static str,
    /// Title of the import panel.
    pub import_title: &'static str,
    /// Label of the configuration file field.
    pub import_path_label: &'static str,
    /// Placeholder in the configuration file field.
    pub import_path_placeholder: &'static str,
    /// Button that reads the file and shows what would be imported.
    pub import_preview: &'static str,
    /// Label introducing the path the preview really read.
    pub import_source_label: &'static str,
    /// Note that the configuration file is only read.
    pub import_read_only: &'static str,
    /// Title of the limits block shown before applying.
    pub import_limits_title: &'static str,
    /// Shown when the file was applied without limitation.
    pub import_limits_none: &'static str,
    /// Label for hosts the importer understood completely.
    pub import_outcome_ready: &'static str,
    /// Label for hosts that can be imported but need a review.
    pub import_outcome_review: &'static str,
    /// Label for hosts that are not imported because their path cannot be trusted.
    pub import_outcome_blocked: &'static str,
    /// Label for hosts whose name a saved host already has, so importing skips them.
    pub import_outcome_saved: &'static str,
    /// Note on a host that will not be imported.
    pub import_not_offered: &'static str,
    /// Label introducing directives this build does not apply.
    pub import_ignored_label: &'static str,
    /// Label introducing the Include lines, whose files were not read.
    pub import_include_label: &'static str,
    /// Label introducing the Match blocks, whose conditions were not evaluated.
    pub import_match_label: &'static str,
    /// Label introducing proxy or forwarding rules no host in the file matched.
    pub import_unmatched_proxy_label: &'static str,
    /// Shown when the file holds no host that could be imported.
    pub import_nothing: &'static str,
    /// Shown when every host the file could import is already saved.
    pub import_all_saved: &'static str,
    /// Button that saves the hosts the preview offers.
    pub import_apply: &'static str,
    /// Prefix of the message left after an import.
    pub import_applied: &'static str,
    /// Unit used after the number of hosts that need a review.
    pub import_review_count: &'static str,
    /// Unit used after the number of hosts that were skipped.
    pub import_skipped: &'static str,
    /// Title of the block shown when the file could not be read or parsed.
    pub import_error_title: &'static str,
    /// Shown when the import panel was asked to read an empty path.
    pub import_path_required: &'static str,

    /// Label of the password authentication method.
    pub auth_password: &'static str,
    /// Label of the plain private key method.
    pub auth_key: &'static str,
    /// Label of the passphrase protected key method.
    pub auth_key_passphrase: &'static str,
    /// Label of the system agent method.
    pub auth_agent: &'static str,
    /// What the password field is for.
    pub auth_password_hint: &'static str,
    /// What the key path field is for.
    pub auth_key_hint: &'static str,
    /// What the passphrase field is for.
    pub auth_key_passphrase_hint: &'static str,
    /// What the agent method uses.
    pub auth_agent_hint: &'static str,
    /// Shown when the password field is empty.
    pub auth_missing_password: &'static str,
    /// Shown when the key path field is empty.
    pub auth_missing_key: &'static str,
    /// Shown when the passphrase field is empty.
    pub auth_missing_passphrase: &'static str,
    /// Button that starts a remote session.
    pub remote_connect: &'static str,
    /// Status before an attempt started.
    pub remote_idle: &'static str,
    /// Status while the transport connects.
    pub remote_connecting: &'static str,
    /// Status while the user decides about the server identity.
    pub remote_verifying: &'static str,
    /// Status of a live remote session.
    pub remote_live: &'static str,
    /// Status of a failed attempt.
    pub remote_failed: &'static str,
    /// Said while the attempt is waiting for the user.
    pub remote_waiting: &'static str,
    /// What that wait is about when the server identity is unknown.
    pub remote_wait_identity: &'static str,
    /// Ends the attempt the dialog is about.
    pub remote_cancel_attempt: &'static str,
    /// Title of the unknown host identity question.
    pub trust_title: &'static str,
    /// Explanation shown beside the fingerprint.
    pub trust_body: &'static str,
    /// Label of the key algorithm.
    pub trust_algorithm: &'static str,
    /// Label of the SHA-256 fingerprint.
    pub trust_fingerprint: &'static str,
    /// Button that stores the identity and connects.
    pub trust_accept: &'static str,
    /// Button that refuses the identity.
    pub trust_reject: &'static str,
    /// Shown after an identity was refused.
    pub trust_rejected_note: &'static str,
    /// Shown when the attempt ended before the acceptance could reach it.
    pub trust_undelivered_note: &'static str,
    /// Title of the failed attempt panel.
    pub remote_failure_title: &'static str,
    /// Button that closes the remote panel.
    pub remote_close: &'static str,
    /// Kind label of a shell on this machine, on the session tab and bar.
    pub session_kind_local: &'static str,
    /// Kind label of an SSH session, on the session tab and bar.
    pub session_kind_remote: &'static str,
    /// Kind label of an RDP or VNC session, on the session tab and bar.
    pub session_kind_desktop: &'static str,
    /// Kind label of a serial session, on the session tab and bar.
    pub session_kind_serial: &'static str,
    /// New or edit title of a connection that is not an SSH host.
    pub connection_editor_new: &'static str,
    /// Title of the form for a saved desktop or serial connection.
    pub connection_editor_edit: &'static str,
    /// Field label: the protocol a new connection speaks.
    pub connection_field_protocol: &'static str,
    /// Field label: the Windows domain a RDP user belongs to.
    pub connection_field_domain: &'static str,
    /// Field label: what a RDP connection does with an untrusted certificate.
    pub connection_field_certificate: &'static str,
    /// Field label: how much a VNC connection insists on encryption.
    pub connection_field_security: &'static str,
    /// Field label: the device a serial connection opens.
    pub connection_field_device: &'static str,
    /// Field label: the line speed of a serial connection.
    pub connection_field_baud: &'static str,
    /// Field label: how many data bits one serial frame carries.
    pub connection_field_data_bits: &'static str,
    /// Field label: how a serial frame carries parity.
    pub connection_field_parity: &'static str,
    /// Field label: how many stop bits end a serial frame.
    pub connection_field_stop_bits: &'static str,
    /// Field label: how a serial port paces its bytes.
    pub connection_field_flow: &'static str,
    /// Field label: whether typed characters are echoed locally.
    pub connection_field_local_echo: &'static str,
    /// Field label: whether a desktop connection shares the clipboard.
    pub connection_field_clipboard: &'static str,
    /// Field label: the local folder a RDP connection maps as a remote drive.
    pub connection_field_drive_folder: &'static str,
    /// Placeholder of the mapped folder: an empty field maps nothing.
    pub connection_drive_folder_placeholder: &'static str,
    /// Note under the mapped folder: what the remote side sees and cannot reach.
    pub connection_drive_folder_note: &'static str,
    /// Note under the serial fields: nothing opens until the user starts a session.
    pub connection_not_started_note: &'static str,
    /// Suffix given to a copied connection's name.
    pub connection_copy_suffix: &'static str,
    /// Button that deletes one saved connection.
    pub connection_delete: &'static str,
    /// Question asked before a connection is deleted.
    pub connection_delete_question: &'static str,
    /// Entry that copies a saved connection into a new one.
    pub connection_duplicate: &'static str,
    /// Message after a desktop or serial connection was written.
    pub connection_saved: &'static str,
    /// Message after a desktop or serial connection was removed.
    pub connection_deleted: &'static str,
    /// Button that closes one session.
    pub session_close: &'static str,
    /// Button that starts another process for a session that ended.
    pub session_reconnect: &'static str,
    /// Why a local session cannot be restarted.
    pub session_reconnect_no_shell: &'static str,
    /// Why a remote session's host cannot be connected to again.
    pub session_reconnect_missing_host: &'static str,
    /// Why a desktop or serial session cannot be started again in this build.
    pub session_reconnect_not_wired: &'static str,
    /// Said where a selected session has no terminal to draw.
    pub session_no_terminal: &'static str,
    /// Headline of a desktop pane that is showing its remote screen.
    pub desktop_live: &'static str,
    /// Headline of a desktop pane whose session ended.
    pub desktop_ended: &'static str,
    pub desktop_credentials_title: &'static str,
    pub desktop_certificate_remember: &'static str,
    pub desktop_certificate_changed: &'static str,
    pub desktop_certificate_title: &'static str,
    pub desktop_certificate_subject: &'static str,
    pub desktop_certificate_issuer: &'static str,
    /// Button that accepts the certificate a desktop server presented.
    pub desktop_certificate_trust: &'static str,
    /// Button that refuses the certificate a desktop server presented.
    pub desktop_certificate_reject: &'static str,
    /// Step of a desktop connection: the socket is being opened.
    pub desktop_stage_connecting: &'static str,
    /// Step of a desktop connection: the server is waiting for a certificate answer.
    pub desktop_stage_certificate: &'static str,
    /// Step of a desktop connection: the credentials are being decided.
    pub desktop_stage_signing_in: &'static str,
    /// Step of a desktop connection: the desktop itself is being built.
    pub desktop_stage_establishing: &'static str,
    /// Elapsed time beside the target while a connection is pending.
    pub desktop_wait_time: &'static str,
    pub desktop_hide_controls: &'static str,
    #[allow(dead_code)]
    pub desktop_show_controls: &'static str,
    /// Control that makes the desktop fill the whole window.
    pub desktop_fullscreen_enter: &'static str,
    /// Control that gives the window back its own layout.
    pub desktop_fullscreen_exit: &'static str,
    /// Zoom control: show the whole desktop in the space it has.
    pub desktop_zoom_fit: &'static str,
    /// Zoom control: one step larger.
    pub desktop_zoom_in: &'static str,
    /// Zoom control: one step smaller.
    pub desktop_zoom_out: &'static str,
    /// Notice: the server refused the clipboard.
    pub desktop_notice_clipboard_refused: &'static str,
    /// Notice: the VNC server's clipboard only carries Latin-1 text.
    pub desktop_notice_clipboard_latin1: &'static str,
    /// Notice: clipboard text over the bound was cut.
    pub desktop_notice_text_too_large: &'static str,
    /// Notice: a clipboard image over the bound was left out.
    pub desktop_notice_image_too_large: &'static str,
    /// Notice: the mapped folder was not offered; `{reason}` is one of the drive reasons.
    pub desktop_notice_drive_unavailable: &'static str,
    /// Notice: the server did not take the mapped folder.
    pub desktop_notice_drive_refused: &'static str,
    /// Drive reason: the folder does not exist.
    pub desktop_drive_missing: &'static str,
    /// Drive reason: the path names a file.
    pub desktop_drive_not_a_folder: &'static str,
    /// Drive reason: the path is not a full path.
    pub desktop_drive_not_absolute: &'static str,
    /// Drive reason: the path is longer than the drive can address.
    pub desktop_drive_too_long: &'static str,
    /// Drive reason: the folder cannot be read.
    pub desktop_drive_no_access: &'static str,
    /// Drive reason: the drive could not be set up.
    pub desktop_drive_not_added: &'static str,
    /// Drive reason: a reason this build does not know.
    pub desktop_drive_other: &'static str,
    /// Button that puts the desktop notices away.
    pub desktop_notice_dismiss: &'static str,
    /// Label of the switch that echoes typed characters locally in a serial pane.
    pub serial_echo: &'static str,
    /// The local echo is on.
    pub serial_echo_on: &'static str,
    /// The local echo is off.
    pub serial_echo_off: &'static str,
    /// Label of the control that chooses what the Return key sends.
    pub serial_newline: &'static str,
    /// Control that asks the system for the serial ports again.
    pub serial_ports_refresh: &'static str,
    /// Headline of a serial port list that could not be read.
    pub serial_ports_failed: &'static str,
    /// Headline of an answer that listed no serial ports at all.
    pub serial_ports_empty: &'static str,
    /// Headline of a port list that does not hold the saved device.
    pub serial_ports_missing: &'static str,
    /// Lead-in of the ports the system did report.
    pub serial_ports_attached: &'static str,
    /// What to do about a device that is not attached, in the port list row.
    pub serial_ports_hint: &'static str,
    /// Note that a serial session closed while its port was still held.
    pub serial_still_held: &'static str,

    /// Button that opens the file view of a remote session.
    pub files_open: &'static str,
    /// Button that lists the parent directory.
    pub files_up: &'static str,
    /// Button that lists the current directory again.
    pub files_refresh: &'static str,
    /// Row that leads to the parent directory.
    pub files_parent_row: &'static str,
    /// Shown while a directory is being read.
    pub files_loading: &'static str,
    /// Shown when a directory is really empty.
    pub files_empty: &'static str,
    /// Next step under the empty-directory line.
    pub files_empty_hint: &'static str,
    /// Names the folder the other side of the file view is showing while one side is
    /// hidden (a narrow window).
    pub files_other_side: &'static str,
    /// Shown while no path is known yet.
    pub files_unknown_path: &'static str,
    /// Shown when a directory could not be read.
    pub files_failed: &'static str,
    /// Said beside a file view failure, so the terminal is not read as failed too.
    pub files_terminal_note: &'static str,
    /// Said when this file source failed and the terminal is still connected.
    ///
    /// Refresh is the recovery: the session is not ended, so reconnect would do nothing.
    pub files_refresh_source: &'static str,
    /// Kind label of a directory entry.
    pub files_kind_directory: &'static str,
    /// Kind label of a file entry.
    pub files_kind_file: &'static str,
    /// Kind label of a symbolic link entry.
    pub files_kind_link: &'static str,
    /// Kind label of any other entry.
    pub files_kind_other: &'static str,
    /// Button that opens the new-folder form.
    pub files_new: &'static str,
    /// Button that opens the rename form of one entry.
    pub files_rename: &'static str,
    /// Button that asks the delete question about one entry.
    pub files_delete: &'static str,
    /// Placeholder of the field that names a new folder.
    pub files_new_placeholder: &'static str,
    /// Placeholder of the field that holds the new name of an entry.
    pub files_rename_placeholder: &'static str,
    /// Button that sends the new directory.
    pub files_create: &'static str,
    /// Button that sends the rename.
    pub files_rename_confirm: &'static str,
    /// Button that sends the deletion.
    pub files_delete_confirm: &'static str,
    /// Button that closes a form without sending anything.
    pub files_cancel: &'static str,
    /// Label introducing the full path a question is about.
    pub files_target_label: &'static str,
    /// Label introducing the kind of the entry a question is about.
    pub files_kind_label: &'static str,
    /// Said about a directory that is deleted with everything inside it.
    pub files_delete_recursive: &'static str,
    /// Said about a file that is deleted.
    pub files_delete_file: &'static str,
    /// Said while a change waits for the server.
    pub files_change_pending: &'static str,
    /// Heading of a change the server applied completely.
    pub files_change_done: &'static str,
    /// Heading of a change the server refused in part.
    pub files_change_failed: &'static str,
    /// Word before the count of paths a change really reached.
    pub files_change_changed: &'static str,
    /// Reason a change cannot be asked for yet.
    pub files_problem_no_directory: &'static str,
    /// Reason: the name is empty.
    pub files_problem_empty: &'static str,
    /// Reason: the name is "." or "..".
    pub files_problem_navigation: &'static str,
    /// Reason: the name holds a separator.
    pub files_problem_separator: &'static str,
    /// Reason: the name is already in the listing.
    pub files_problem_taken: &'static str,
    /// Reason: the entry is not in the listing any more.
    pub files_problem_missing: &'static str,
    /// Reason: the new name is the old one.
    pub files_problem_unchanged: &'static str,
    /// Narrow-window button that shows the remote column.
    pub files_show_remote: &'static str,
    /// Said while following waits for a shell that has not reported a directory.
    pub files_follow_no_report: &'static str,
    /// Said after browsing by hand turned following off.
    pub files_follow_manual: &'static str,
    /// Said when the reported directory could not be read. The path is appended.
    pub files_follow_unreadable: &'static str,
    /// The switch that makes the remote column follow the shell's own directory, and
    /// its state while it is on, in its tooltip (R15).
    pub files_follow_on: &'static str,
    /// The follow switch's state while it is off, in its tooltip (R15).
    pub files_follow_off: &'static str,
    /// Button that uploads one local file into the remote directory on screen.
    pub files_to_remote: &'static str,
    /// Button that downloads one remote file into the local directory on screen.
    pub files_to_local: &'static str,
    /// Said about a directory deleted on this computer, including its contents.
    pub files_delete_recursive_local: &'static str,
    /// Said about a file deleted on this computer.
    pub files_delete_file_local: &'static str,
    /// Said while a local change has not been listed again.
    pub files_change_pending_local: &'static str,
    /// Heading when this computer applied a change.
    pub files_change_done_local: &'static str,
    /// Heading when this computer did not apply every part of a change.
    pub files_change_failed_local: &'static str,
    /// Local failure: the path is not there. The path is appended.
    pub files_local_not_found: &'static str,
    /// Local failure: this account may not read the path. The path is appended.
    pub files_local_denied: &'static str,
    /// Local failure: the path is not a directory. The path is appended.
    pub files_local_not_directory: &'static str,
    /// Local failure: the disk or volume went away. The reason is appended.
    pub files_local_lost: &'static str,
    /// Local failure: the computer refused the request. The reason is appended.
    pub files_local_refused: &'static str,
    /// Local failure: the name is already taken. The path is appended.
    pub files_local_exists: &'static str,
    /// Local failure: the folder cannot be listed at all. The reason is appended.
    pub files_local_unavailable: &'static str,
    /// Label of the root breadcrumb on this computer.
    pub files_local_root: &'static str,
    /// Tooltip of the name header while the column is sorted by name, A to Z.
    pub files_sort_asc: &'static str,
    /// Tooltip of the name header while the column is sorted by name, Z to A.
    pub files_sort_desc: &'static str,
    /// Tooltip of the size header while the column is sorted by size, smallest first.
    pub files_sort_size_asc: &'static str,
    /// Tooltip of the size header while the column is sorted by size, largest first.
    pub files_sort_size_desc: &'static str,
    /// Tooltip of the modified header while the column is sorted newest first.
    pub files_sort_modified_new: &'static str,
    /// Tooltip of the modified header while the column is sorted oldest first.
    pub files_sort_modified_old: &'static str,
    /// Placeholder of the field that filters one column's names.
    pub files_filter_placeholder: &'static str,
    /// How many names the filter lets through, out of how many the listing holds.
    pub files_filter_count: &'static str,
    /// How many names the listing holds, while no filter narrows it.
    pub files_item_count: &'static str,
    /// Button that clears the filter of one column.
    pub files_filter_clear: &'static str,
    /// Said instead of the listing when the filter matches no name.
    pub files_filter_none: &'static str,
    /// How many entries one column has marked.
    pub files_marked_count: &'static str,
    /// Button that marks every name the filter lets through.
    pub files_mark_all: &'static str,
    /// Button that drops the marks of one column.
    pub files_clear_marks: &'static str,
    /// Button that uploads every marked local file. The count is filled in.
    pub files_upload_selected: &'static str,
    /// Button that downloads every marked remote file. The count is filled in.
    pub files_download_selected: &'static str,
    /// Button that deletes every marked entry. The count is filled in.
    pub files_delete_selected: &'static str,
    /// Menu line that deletes the whole marked batch, said where the row is right-clicked.
    pub files_delete_marked: &'static str,
    /// Button that copies the paths of every marked entry. The count is filled in.
    pub files_copy_selected: &'static str,
    /// How the marks are made, said beside them.
    pub files_marked_hint: &'static str,
    /// What deleting several entries really covers. The counts are filled in.
    pub files_delete_many_note: &'static str,
    /// What a multi-entry delete question is about.
    pub files_delete_many_target: &'static str,
    /// Button that shows the terminal and the files side by side again.
    pub files_mode_dock: &'static str,
    /// Button that gives the whole area to the files.
    pub files_mode_focus: &'static str,
    /// Toast: several uploads were queued. The count is filled in.
    pub files_uploaded_selected: &'static str,
    /// Toast: several downloads were queued. The count is filled in.
    pub files_downloaded_selected: &'static str,
    /// Beside the transfer list: how many folders a batch left out. The count is
    /// filled in.
    pub files_batch_folders_skipped: &'static str,
    /// Heading of the transfer list of a session.
    pub transfers_heading: &'static str,
    /// Said of a task the connection has taken but whose first byte has not arrived.
    pub transfers_queued_note: &'static str,
    /// The time the missing bytes take at the measured speed; the duration is filled in.
    pub transfers_remaining: &'static str,
    /// Said when a session has no transfer yet.
    pub transfers_empty: &'static str,
    /// Button that asks for the local file to upload.
    pub transfers_upload: &'static str,
    /// Button that sends an upload.
    pub transfers_upload_confirm: &'static str,
    /// Placeholder of the field that holds the local file to upload.
    pub transfers_upload_placeholder: &'static str,
    /// Button that asks where one entry is downloaded to.
    pub transfers_download: &'static str,
    /// Button that sends a download.
    pub transfers_download_confirm: &'static str,
    /// Placeholder of the field that holds the local folder.
    pub transfers_download_placeholder: &'static str,
    /// Button that stops a transfer.
    pub transfers_cancel: &'static str,
    /// Button that starts a new task for one that did not finish.
    pub transfers_retry: &'static str,
    /// Word before the partial file a failed transfer left behind.
    pub transfers_residue: &'static str,
    /// Question asked before an existing target is replaced.
    pub transfers_overwrite_question: &'static str,
    /// Answer that replaces the existing target.
    pub transfers_overwrite_replace: &'static str,
    /// Answer that keeps the existing target.
    pub transfers_overwrite_keep: &'static str,
    /// Reason: the path is empty.
    pub transfers_problem_empty_path: &'static str,
    /// Reason: the path names no file.
    pub transfers_problem_no_name: &'static str,
    /// Reason: the source names no file.
    pub transfers_problem_no_source: &'static str,
    /// Reason: no directory is known for the target.
    pub transfers_problem_no_directory: &'static str,
    /// Reason: the connection cannot take the command.
    pub transfers_problem_no_connection: &'static str,
    /// Reason: the source is a directory and recursion was not chosen.
    pub transfers_problem_needs_recursion: &'static str,
    /// Reason: the source is a directory and the chosen protocol copies one file.
    pub transfers_problem_directory_needs_scp: &'static str,
    /// Label of the protocol switch in a transfer question.
    pub transfers_protocol: &'static str,
    /// Protocol name of the SFTP subsystem.
    pub transfers_protocol_sftp: &'static str,
    /// Protocol name of the traditional SCP protocol.
    pub transfers_protocol_scp: &'static str,
    /// The sentence that says what choosing SCP changes.
    pub transfers_protocol_note: &'static str,
    /// Label of the toggle that copies a directory with its contents.
    pub transfers_recursive: &'static str,
    /// Label of the remote directory an upload would land in.
    pub transfers_remote_directory: &'static str,
    /// Label of the remote path a download would copy.
    pub transfers_remote_path: &'static str,
    /// The full path an upload lands at, said only when it differs from the typed
    /// directory; `{path}` is that path.
    pub transfers_lands_at: &'static str,
    /// Why directory browsing failing does not stop a copy over SCP.
    pub files_sftp_scp_hint: &'static str,
    /// Question asked before a session with running transfers is closed.
    pub transfers_close_question: &'static str,
    /// Answer that keeps a session open.
    pub transfers_close_cancel: &'static str,
    /// Answer that closes the session and cancels its transfers.
    pub transfers_close_confirm: &'static str,
    /// Button that continues a checkpointed partial file.
    pub transfers_resume: &'static str,
    /// Button that starts again from the first byte.
    pub transfers_restart: &'static str,
    /// Word before a confirmed offset that can be resumed.
    pub transfers_resume_from: &'static str,
    /// Button that shows a downloaded file in the system's file manager.
    pub transfers_reveal: &'static str,
    /// Button that removes one finished transfer from the list.
    pub transfers_remove: &'static str,
    /// Resume refused because the source changed.
    pub transfers_resume_source: &'static str,
    /// Resume refused because the partial file could not be checked.
    pub transfers_resume_unverifiable: &'static str,
    /// Resume refused because the target cannot continue a partial file.
    pub transfers_resume_unsupported: &'static str,
    /// Resume refused because the partial file is damaged.
    pub transfers_resume_damaged: &'static str,
    /// Question before the application closes with transfers still running.
    pub transfers_close_app_question: &'static str,
    /// Answer that keeps the application open.
    pub transfers_close_app_cancel: &'static str,
    /// Answer that closes the application and stops its transfers.
    pub transfers_close_app_confirm: &'static str,
    /// Label before the host a transfer belongs to.
    pub transfers_host: &'static str,
    /// Placeholder of the palette's search field.
    pub palette_placeholder: &'static str,
    /// Shown when nothing in the palette matches.
    pub palette_empty: &'static str,
    /// Palette group: open sessions.
    pub palette_group_sessions: &'static str,
    /// Palette group: shells to start.
    pub palette_group_shells: &'static str,
    /// Palette group: saved hosts.
    pub palette_group_hosts: &'static str,
    /// Palette group: pages.
    pub palette_group_navigate: &'static str,
    /// Palette group: window actions.
    pub palette_group_actions: &'static str,
    /// Palette group: saved shortcut commands.
    pub palette_group_snippets: &'static str,
    /// Palette action: close the session on screen.
    pub palette_close_tab: &'static str,
    /// Palette action: switch the interface language.
    pub palette_language: &'static str,
    /// Palette action: step the text size.
    pub palette_text_size: &'static str,
    /// Palette action: drop the current tab's name and go back to its automatic title.
    pub palette_restore_tab_title: &'static str,
    /// Palette action: clear the current tab's distinguishing colour.
    pub palette_clear_tab_accent: &'static str,
    /// Marks the session already on screen.
    pub palette_current: &'static str,
    /// Footer hint: arrow keys.
    pub palette_key_move: &'static str,
    /// Footer hint: Enter.
    pub palette_key_run: &'static str,
    /// Footer hint: Escape.
    pub palette_key_close: &'static str,
    /// Tab menu: open another session like this one.
    pub tab_menu_duplicate: &'static str,
    /// Tab menu: close this tab.
    pub tab_menu_close: &'static str,
    /// Tab menu: close every other tab.
    pub tab_menu_close_others: &'static str,
    /// Tab menu: close the tabs after this one.
    pub tab_menu_close_right: &'static str,
    /// Session menu: show the terminal.
    pub session_menu_open: &'static str,
    /// Session menu: start a closed session again.
    pub session_menu_reopen: &'static str,
    /// Session menu: close the tab, keep the record.
    pub session_menu_close: &'static str,
    /// Session menu: close if needed and remove the record.
    pub session_menu_remove: &'static str,
    /// Session menu and list: remove every closed record.
    pub session_menu_remove_closed: &'static str,
    /// Tooltip of the row's more-actions button.
    pub session_more: &'static str,
    /// List toolbar: how many closed records there are, before the count.
    pub session_closed_summary: &'static str,
    /// Text field menu: cut the selection.
    pub field_cut: &'static str,
    /// Text field menu: copy the selection.
    pub field_copy: &'static str,
    /// Text field menu: paste.
    pub field_paste: &'static str,
    /// Text field menu: select everything.
    pub field_select_all: &'static str,
    /// Tooltip: show a hidden password.
    pub field_reveal: &'static str,
    /// Tooltip: hide the password again.
    pub field_conceal: &'static str,
    /// Button beside a path field that opens the system file chooser.
    pub field_browse: &'static str,
    /// Host dialog: heading of the sign-in choice.
    pub host_auth_title: &'static str,
    /// Host dialog: label of the key passphrase field.
    pub host_secret_passphrase: &'static str,
    /// Host dialog: checkbox that keeps the password.
    pub host_remember_password: &'static str,
    /// Host dialog: checkbox that keeps the passphrase.
    pub host_remember_passphrase: &'static str,
    /// Host dialog: where a remembered secret is kept.
    pub host_remember_note: &'static str,
    /// Host dialog: placeholder of the secret field when one is saved.
    pub host_secret_saved_placeholder: &'static str,
    /// Host dialog: the system store refused the secret; the reason follows.
    pub host_secret_failed: &'static str,
    /// Host dialog: label of the startup command.
    pub host_startup_label: &'static str,
    /// Host dialog: placeholder of the startup command.
    pub host_startup_placeholder: &'static str,
    /// Host dialog: what the startup command does.
    pub host_startup_note: &'static str,
    /// Label of the X11 forwarding choice (R11).
    pub x11_label: &'static str,
    /// The three X11 modes, as a chooser shows them.
    pub x11_off_label: &'static str,
    /// The mode bound by the X11 SECURITY extension, like `ssh -X`.
    pub x11_restricted_label: &'static str,
    /// The mode without those limits, like `ssh -Y`.
    pub x11_trusted_label: &'static str,
    /// What X11 forwarding is and why off is the default.
    pub x11_note: &'static str,
    /// Heading of the per-connection X11 choice.
    pub x11_connect_label: &'static str,
    /// Title of the trusted-mode risk confirmation.
    pub x11_trusted_title: &'static str,
    /// What the trusted mode lets a remote program do.
    pub x11_trusted_body: &'static str,
    /// Answer that turns the trusted mode on for this connection.
    pub x11_trusted_confirm: &'static str,
    /// Answer that keeps forwarding but with the security extension limits.
    pub x11_trusted_restricted: &'static str,
    /// Answer that does not connect at all.
    pub x11_trusted_cancel: &'static str,
    /// Session bar: no X11 forwarding for this session.
    pub x11_bar_off: &'static str,
    /// Session bar: the request is on its way.
    pub x11_bar_enabling: &'static str,
    /// Session bar: forwarding is available.
    pub x11_bar_ready: &'static str,
    /// Session bar: forwarding could not be used.
    pub x11_bar_failed: &'static str,
    /// Host dialog: save and open a connection at once.
    pub host_save_connect: &'static str,
    /// Connection form: a remembered secret could not be read; the reason follows.
    pub remote_saved_secret_failed: &'static str,
    /// Theme value: follow the operating system.
    pub theme_system: &'static str,
    /// Theme value: light.
    pub theme_light: &'static str,
    /// Theme value: dark.
    pub theme_dark: &'static str,
    /// Palette action: step the theme.
    pub palette_theme: &'static str,
    /// Motion value: animations play.
    pub motion_full: &'static str,
    /// Motion value: animations are skipped.
    pub motion_reduced: &'static str,
    /// Palette action: switch reduced motion.
    pub palette_motion: &'static str,
    /// Opens a connection that is not saved as a host first.
    pub quick_connect: &'static str,
    /// Explains that a quick connection does not write a host record.
    pub quick_connect_hint: &'static str,
    /// Title of the quick-connection form.
    pub quick_connect_title: &'static str,
    /// Saves a successful quick connection as a host.
    pub quick_save_host: &'static str,
    /// Shown after that host was written.
    pub quick_saved: &'static str,
    /// Shown while a quick connection has not been saved.
    pub quick_unsaved_note: &'static str,
    /// Heading above connection attempts that are not the one on screen.
    pub attempt_others: &'static str,
    /// Cancels one connection attempt.
    pub attempt_cancel: &'static str,
    /// Palette group: hosts that were really reached, newest first.
    pub palette_group_recent: &'static str,
    /// Label of the current session in the identity bar.
    /// Quick connect: the port is not a number from 1 to 65535.
    pub quick_port_invalid: &'static str,
    /// Quick connect: what to show when the port field is blank.
    pub quick_port_blank: &'static str,
    /// Quick connect: the port field is the one to change.
    pub quick_fix_port: &'static str,
    /// Quick connect: a blank address left the connection with no name.
    pub quick_invalid_name: &'static str,
    /// Quick connect: the address field is empty.
    pub quick_invalid_address: &'static str,
    /// Quick connect: the address field contains whitespace.
    pub quick_invalid_address_space: &'static str,
    /// Quick connect: the user field is empty.
    pub quick_invalid_user: &'static str,
    /// Shown after a quick connection was saved, as the next step.
    pub quick_saved_next: &'static str,
    /// The host file could not be written while saving a quick connection.
    pub quick_save_write_failed: &'static str,
    /// What to do after that write failed.
    pub quick_save_write_next: &'static str,
    /// A quick connection's identifier is already a saved host.
    pub quick_save_duplicate: &'static str,
    /// What to do when that identifier is already saved.
    pub quick_save_duplicate_next: &'static str,
    /// The host to update is not in the file.
    pub quick_save_unknown: &'static str,
    /// What to do when the host is not in the file.
    pub quick_save_unknown_next: &'static str,
    /// The saved hosts disagree, so this one was not written.
    pub quick_save_inconsistent: &'static str,
    /// What to do when the saved hosts disagree.
    pub quick_save_inconsistent_next: &'static str,
    /// Host dialog: heading of the ordered jump list.
    pub host_jump_title: &'static str,
    /// Host dialog: shown when the jump list is empty.
    pub host_jump_empty: &'static str,
    /// Host dialog: opens the picker that adds a saved host to the jump list.
    pub host_jump_add: &'static str,
    /// Host dialog: moves a jump earlier.
    pub host_jump_up: &'static str,
    /// Host dialog: moves a jump later.
    pub host_jump_down: &'static str,
    /// Host dialog: removes a jump.
    pub host_jump_remove: &'static str,
    /// Host dialog: label of the ProxyCommand field.
    pub host_proxy_label: &'static str,
    /// Host dialog: placeholder of the ProxyCommand field.
    pub host_proxy_placeholder: &'static str,
    /// Host dialog: the command is stored and not run from the form.
    pub host_proxy_note: &'static str,
    /// Host dialog: both a jump list and a ProxyCommand are set.
    pub host_proxy_conflict: &'static str,
    /// Connection form: the node the missing credentials belong to.
    pub remote_asking_hop: &'static str,
    /// Connection form: title of the ProxyCommand confirmation.
    pub remote_proxy_title: &'static str,
    /// Connection form: run the expanded ProxyCommand.
    pub remote_proxy_confirm: &'static str,
    /// Connection form: do not run the ProxyCommand.
    pub remote_proxy_refuse: &'static str,
    /// Import preview: label in front of a ProxyCommand.
    pub import_proxy_label: &'static str,
    /// Import preview: label in front of a jump hop.
    pub import_jump_label: &'static str,
    /// Import preview: label in front of a forwarding rule.
    pub import_forward_label: &'static str,
    /// Import: hosts were written and the forwarding file was not.
    pub import_forwards_failed: &'static str,
    /// Pane: split the active pane to the right.
    pub pane_split_right: &'static str,
    /// Pane: split the active pane downward.
    pub pane_split_down: &'static str,
    /// Pane: the active pane fills its tab.
    pub pane_zoom: &'static str,
    /// Pane: leave zoom and show the other panes.
    pub pane_unzoom: &'static str,
    /// Marks the pane that receives the keyboard.
    pub pane_active: &'static str,
    /// Names one pane of a split by its slot: the word, then the number.
    pub pane_label: &'static str,
    /// A restored pane that has not been started.
    pub pane_pending: &'static str,
    /// Explains that a pending pane has no process and no old output.
    pub pane_pending_body: &'static str,
    /// Starts the pane the user is looking at. Nothing starts before this.
    pub pane_connect: &'static str,
    /// Starts every waiting local pane of the tab at once.
    pub pane_connect_all: &'static str,
    /// Removes one pane.
    pub pane_remove: &'static str,
    /// Closes one pane and leaves the others running.
    pub pane_close: &'static str,
    /// Shrinks the active pane along its split.
    pub pane_resize_less: &'static str,
    /// Grows the active pane along its split.
    pub pane_resize_more: &'static str,
    /// Tab menu: move this tab toward the start.
    pub tab_menu_move_left: &'static str,
    /// Tab menu: move this tab toward the end.
    pub tab_menu_move_right: &'static str,
    /// Tab menu: give this tab a name of its own.
    pub tab_menu_rename: &'static str,
    /// Title of the tab-name dialog.
    pub tab_rename_title: &'static str,
    /// Placeholder of the tab-name field.
    pub tab_rename_placeholder: &'static str,
    /// Explains what a tab name replaces, and what it never replaces.
    pub tab_rename_hint: &'static str,
    /// Saves the tab name.
    pub tab_rename_save: &'static str,
    /// Why an empty name was refused.
    pub tab_rename_empty: &'static str,
    /// Clears the model name and goes back to the automatic title.
    pub tab_rename_auto: &'static str,
    /// Says, while the name is too long, that saving keeps the first `{max}` characters.
    pub tab_rename_limit: &'static str,
    /// Says, after such a save, what was kept and how to change it.
    pub tab_rename_shortened: &'static str,
    /// Heading above the colour choices of one tab.
    pub tab_accent_label: &'static str,
    /// No colour chosen for this tab.
    pub tab_accent_none: &'static str,
    /// Tab colour: blue.
    pub tab_accent_iris: &'static str,
    /// Tab colour: violet.
    pub tab_accent_violet: &'static str,
    /// Tab colour: teal.
    pub tab_accent_teal: &'static str,
    /// Tab colour: yellow-green.
    pub tab_accent_lime: &'static str,
    /// Tab colour: orchid pink.
    pub tab_accent_orchid: &'static str,
    /// Tab colour: warm clay.
    pub tab_accent_clay: &'static str,
    /// Title of the list of every open tab.
    pub tab_list_title: &'static str,
    /// The button at the strip's left end while tabs are hidden past it (R2).
    pub tabs_scroll_start: &'static str,
    /// The button at the strip's right end while tabs are hidden past it (R2).
    pub tabs_scroll_end: &'static str,
    /// Explains the tab list: strip order, and choosing one shows it.
    pub tab_list_hint: &'static str,
    /// Mark on a tab whose shell rang the bell in the background.
    pub tab_mark_bell: &'static str,
    /// Mark on a tab where a shell-reported command ended in the background.
    pub tab_mark_finished: &'static str,
    /// Tooltip of the bell mark: what it means and that viewing clears it.
    pub tab_mark_bell_hint: &'static str,
    /// Tooltip of the command-end mark: what it means and that viewing clears it.
    pub tab_mark_finished_hint: &'static str,
    /// A host id in a pane is not in the saved book.
    pub pane_missing_host: &'static str,
    /// What to do about a pane whose host is missing.
    pub pane_missing_next: &'static str,
    /// The saved active tab id is not in the workspace.
    pub workspace_dangling_tab: &'static str,
    /// The saved active pane id is not on its tab.
    pub workspace_dangling_pane: &'static str,
    /// Classic terminal mode, restored as a preference only.
    pub pane_mode_classic: &'static str,
    /// Command-block mode, restored as a preference. Output is not brought back.
    pub pane_mode_integrated: &'static str,
    /// Toolbar: show command blocks when the shell reports them.
    pub terminal_mode_blocks: &'static str,
    /// Command block that has not ended.
    pub terminal_block_running: &'static str,
    /// Command block whose exit code was not reported.
    pub terminal_block_unknown: &'static str,
    /// Label before a reported exit code.
    pub terminal_block_exit: &'static str,
    /// Copies one command block.
    pub terminal_block_copy: &'static str,
    /// Runs the block's command again, as a new execution.
    pub terminal_block_rerun: &'static str,
    /// Opens search of this terminal's history and command blocks.
    pub terminal_search: &'static str,
    /// Shown when a search matches nothing in this terminal.
    pub terminal_search_empty: &'static str,
    /// Shown when a query cannot be read as a regular expression.
    pub terminal_search_invalid: &'static str,
    /// Moves to the next history match.
    pub terminal_search_next: &'static str,
    /// Moves to the previous history match.
    pub terminal_search_previous: &'static str,
    /// Switch: a match must also agree on upper and lower case.
    pub terminal_search_case: &'static str,
    /// Switch: the query is a regular expression instead of literal text.
    pub terminal_search_regex: &'static str,
    /// Closes the history search.
    pub terminal_search_close: &'static str,
    /// The keys the bar answers while it holds the keyboard.
    pub terminal_search_keys: &'static str,
    /// Shown while the bar is open but the terminal has the keyboard again.
    pub terminal_search_keyboard_terminal: &'static str,
    /// Title of the paste confirmation.
    pub terminal_paste_title: &'static str,
    /// Label for the session a paste would be sent to.
    pub terminal_paste_target: &'static str,
    /// Confirms a paste into the session named above it.
    pub terminal_paste_confirm: &'static str,
    /// Drops a paste without sending it.
    pub terminal_paste_cancel: &'static str,
    /// Unit after the number of lines in a paste.
    pub terminal_paste_lines: &'static str,
    /// Line of the confirmation that says a saved command runs once it is confirmed.
    pub terminal_paste_runs: &'static str,
    /// Confirms a saved command that will run once it is written.
    pub terminal_paste_run_confirm: &'static str,
    /// What leaving the confirmation does: nothing is written.
    pub terminal_paste_safe: &'static str,
    /// Enhanced mode, but the shell has not emitted integration markers.
    pub terminal_no_blocks: &'static str,
    /// Integration was seen and then stopped being trustworthy.
    pub terminal_blocks_failed: &'static str,
    /// Filter: every command block.
    pub terminal_filter_all: &'static str,
    /// Filter: commands that have not ended.
    pub terminal_filter_running: &'static str,
    /// Filter: commands with no exit code.
    pub terminal_filter_unknown: &'static str,
    /// Filter: commands that exited non-zero.
    pub terminal_filter_failed: &'static str,
    /// Filter: commands that exited zero. Not a generic success.
    pub terminal_filter_zero: &'static str,
    /// Navigation entry: forwarding rules.
    pub nav_forwards: &'static str,
    /// One-line description of the forwarding page.
    pub nav_forwards_summary: &'static str,
    /// Empty forwarding page title.
    pub empty_forwards_title: &'static str,
    /// Empty forwarding page body. Restore does not listen.
    pub empty_forwards_body: &'static str,
    /// Runtime: saved, not started.
    pub forward_saved: &'static str,
    /// Runtime: start sent, not confirmed.
    pub forward_starting: &'static str,
    /// Runtime: the listener is up.
    pub forward_listening: &'static str,
    /// Runtime: the user stopped it.
    pub forward_stopped: &'static str,
    /// Runtime: the last start failed.
    pub forward_failed: &'static str,
    /// Starts the selected rule.
    pub forward_start: &'static str,
    /// Stops the selected rule.
    pub forward_stop: &'static str,
    /// Saves the form without starting it.
    pub forward_save: &'static str,
    /// Deletes a saved rule.
    pub forward_delete: &'static str,
    /// Edits a saved rule.
    pub forward_edit: &'static str,
    /// Title of the non-loopback confirmation.
    pub forward_exposure_title: &'static str,
    /// Explains a listen on every interface.
    pub forward_exposure_all: &'static str,
    /// Explains a listen on one non-loopback address.
    pub forward_exposure_named: &'static str,
    /// Confirms the exposure and only then starts.
    pub forward_exposure_confirm: &'static str,
    /// Cancels a non-loopback start. The rule stays saved.
    pub forward_exposure_cancel: &'static str,
    /// Close question: these rules stop with this connection.
    pub forward_close_rules: &'static str,
    /// Close question for the application when forwards are listening.
    pub forward_close_app: &'static str,
    /// Button that opens the forwarding page for one host or session.
    pub forward_open: &'static str,
    /// The connection carrying a rule closed.
    pub forward_carrier_lost: &'static str,
    /// Shown when a pure forward connection is being opened.
    pub forward_connect_only: &'static str,
}

/// English copy.
pub static ENGLISH: Strings = Strings {
    nav_local: "Terminals",
    nav_local_summary: "Open terminals, local and remote",
    nav_hosts: "Hosts",
    nav_hosts_summary: "Saved servers and SSH settings",
    nav_sessions: "Sessions",
    nav_sessions_summary: "Every open terminal",
    nav_transfers: "Transfers",
    nav_transfers_summary: "Uploads and downloads",
    empty_local_title: "No local shell is open",
    empty_local_body: "A local shell runs in the current directory of this machine and needs no network.",
    empty_hosts_title: "No saved hosts yet",
    empty_hosts_body: "A saved host keeps the address, port and user so a connection does not have to be retyped.",
    empty_sessions_title: "No session is open",
    empty_sessions_body: "Local and remote terminals appear here with their own name, type and state.",
    empty_transfers_title: "No transfer yet",
    empty_transfers_body: "Uploads and downloads list their source, target and progress here.",
    loading_title: "Waiting for real data",
    loading_body: "Nothing is shown here until the system reports what actually exists.",
    failed_title: "This could not be completed",
    recovery_label: "Suggested next step",
    recovery_retry: "Try again",
    recovery_create_host: "Create a host",
    recovery_import_config: "Import an SSH configuration",
    recovery_choose_shell: "Choose a shell",
    next_step: "Next step",
    primary_hosts: "New host",
    unavailable_notice: "This action is not available in this build, so nothing was started.",
    count_hosts: "hosts",
    count_host_one: "host",
    local_shell_fallback: "System shell",
    shell_custom_button: "Custom path",
    shell_custom_placeholder: "Program name or full path, then Enter",
    shell_none_found: "No shell was found on this machine. Enter a program path to start one.",
    terminal_live: "Running",
    terminal_failed: "Could not start",
    terminal_exited: "Exited",
    terminal_terminated: "Terminated",
    terminal_disconnected: "Disconnected",
    session_details_show: "Show details",
    session_details_hide: "Hide details",
    terminal_environment_unavailable: "Login environment unavailable: this shell starts with the \
         environment the application inherited",
    terminal_environment_unreadable: "The login shell returned no readable environment: this shell \
         starts with the environment the application inherited",
    terminal_environment_locale_replaced: "The locale was not UTF-8: this shell starts in a UTF-8 \
         locale instead",
    terminal_environment_locale_added: "No locale was inherited: this shell starts in a UTF-8 locale",
    terminal_copy: "Copy",
    terminal_smaller: "Smaller text",
    terminal_larger: "Larger text",
    shell_custom_hint: "Any program, by path",
    keys_search_placeholder: "Search actions or shortcuts",
    hosts_view_all: "All hosts",
    hosts_view_recent: "Recent connections",
    hosts_recent_empty: "No connection has been recorded yet. A host appears here after it really connected.",
    hosts_no_match: "No saved host matches this search.",
    host_field_name: "Name",
    host_field_address: "Address",
    host_field_port: "Port",
    host_field_user: "User",
    host_field_group: "Group",
    host_field_identity: "Key file",
    host_identity_note: "Only the path is saved; Shellune never reads or copies the key itself.",
    host_editor_new: "New host",
    host_editor_edit: "Edit host",
    host_save: "Save host",
    host_cancel: "Cancel",
    host_edit: "Edit",
    host_delete: "Delete",
    host_delete_question: "Delete this saved host?",
    host_delete_confirm: "Delete host",
    host_delete_note: "Only the saved record is removed; nothing on the server changes.",
    hosts_saved: "Host saved. No connection was opened.",
    hosts_deleted: "Host deleted.",
    hosts_retry: "Read the file again",
    host_last_connected: "Last connected",
    host_connection_count: "connections",

    import_open: "Import SSH config",
    import_title: "Import an SSH configuration",
    import_path_label: "File",
    import_path_placeholder: "Path to an SSH config file",
    import_preview: "Preview",
    import_source_label: "Read from",
    import_read_only: "The file is only read; Shellune never writes to it.",
    import_limits_title: "Before importing",
    import_limits_none: "Every host in this file was read completely.",
    import_outcome_ready: "Importable",
    import_outcome_review: "Needs review",
    import_outcome_blocked: "Blocked",
    import_outcome_saved: "Already saved",
    import_not_offered: "not imported",
    import_ignored_label: "Not applied",
    import_include_label: "Included files were not read",
    import_match_label: "Match blocks were not evaluated",
    import_unmatched_proxy_label: "Proxy or forwarding rules no host here matches",
    import_nothing: "No host in this file can be imported as a direct connection yet.",
    import_all_saved: "Every host in this file is already saved.",
    import_apply: "Import hosts",
    import_applied: "Imported",
    import_review_count: "need review",
    import_skipped: "skipped",
    import_error_title: "Nothing was imported",
    import_path_required: "Name the configuration file to read.",
    auth_password: "Password",
    auth_key: "Key file",
    auth_key_passphrase: "Key file with passphrase",
    auth_agent: "System agent",
    auth_password_hint: "Used for this attempt only; Shellune never saves it.",
    auth_key_hint: "A readable private key the server authorizes.",
    auth_key_passphrase_hint: "The passphrase decrypts the key for this attempt only.",
    auth_agent_hint: "Signs with a key the system agent already holds.",
    auth_missing_password: "Enter the password for this host.",
    auth_missing_key: "Choose the private key file to use.",
    auth_missing_passphrase: "Enter the passphrase for this key.",
    remote_connect: "Connect",
    remote_idle: "Not connected yet",
    remote_connecting: "Connecting",
    remote_verifying: "Waiting for your decision",
    remote_live: "SSH session live",
    remote_failed: "Could not connect",
    remote_waiting: "Waiting for you",
    remote_wait_identity: "the server's identity",
    remote_cancel_attempt: "Cancel connection",
    trust_title: "This server identity is not known yet",
    trust_body: "Compare this fingerprint with the one the server's administrator can print. Nothing is stored until you accept it.",
    trust_algorithm: "Algorithm",
    trust_fingerprint: "SHA-256 fingerprint",
    trust_accept: "Accept and connect",
    trust_reject: "Reject",
    trust_rejected_note: "The identity was rejected, so no terminal was opened and no trust record was written.",
    trust_undelivered_note: "The connection attempt had already ended, so the identity was neither accepted nor stored. Connect again to see it once more.",
    remote_failure_title: "The connection did not open",
    remote_close: "Close the connection panel",
    session_kind_local: "Local",
    session_kind_remote: "SSH",
    session_kind_desktop: "Desktop",
    session_kind_serial: "Serial",
    connection_editor_new: "New connection",
    connection_editor_edit: "Edit connection",
    connection_field_protocol: "Protocol",
    connection_field_domain: "Domain",
    connection_field_certificate: "Certificate",
    connection_field_security: "Security",
    connection_field_device: "Device",
    connection_field_baud: "Baud rate",
    connection_field_data_bits: "Data bits",
    connection_field_parity: "Parity",
    connection_field_stop_bits: "Stop bits",
    connection_field_flow: "Flow control",
    connection_field_local_echo: "Local echo",
    connection_field_clipboard: "Share the clipboard",
    connection_field_drive_folder: "Map a local folder",
    connection_drive_folder_placeholder: "Leave empty to map no folder",
    connection_drive_folder_note: "The remote desktop sees this folder as the drive Shellune and cannot reach anything outside it.",
    connection_not_started_note: "Saving a connection opens nothing: it dials, or opens the device, only when you start a session.",
    connection_copy_suffix: " (copy)",
    connection_delete: "Delete connection",
    connection_delete_question: "Delete this saved connection? Nothing that is running is affected.",
    connection_duplicate: "Duplicate",
    connection_saved: "Connection saved",
    connection_deleted: "Connection deleted",
    session_close: "Close session",
    session_reconnect: "Reconnect",
    session_reconnect_no_shell: "This session has no program recorded, so it cannot be started again.",
    session_reconnect_missing_host: "The saved host this session used is no longer there, so it cannot be connected to again.",
    session_reconnect_not_wired: "Starting this kind of session is not available yet, so nothing was started.",
    session_no_terminal: "This session has no terminal open",
    desktop_live: "Remote desktop",
    desktop_ended: "The remote desktop session ended",
    desktop_credentials_title: "Sign in to remote desktop",
    desktop_certificate_remember: "Remember this certificate for this host",
    desktop_certificate_changed: "The server certificate has changed. Previously trusted fingerprint",
    desktop_certificate_title: "Verify server certificate",
    desktop_certificate_subject: "Issued to",
    desktop_certificate_issuer: "Issued by",
    desktop_certificate_trust: "Trust and connect",
    desktop_certificate_reject: "Reject",
    desktop_stage_connecting: "Opening the connection",
    desktop_stage_certificate: "Waiting for the certificate decision",
    desktop_stage_signing_in: "Signing in",
    desktop_stage_establishing: "Waiting for the first picture",
    desktop_wait_time: "Waiting",
    desktop_hide_controls: "Hide desktop controls",
    desktop_show_controls: "Show desktop controls",
    desktop_fullscreen_enter: "Full screen",
    desktop_fullscreen_exit: "Leave full screen",
    desktop_zoom_fit: "Fit",
    desktop_zoom_in: "Larger",
    desktop_zoom_out: "Smaller",
    desktop_notice_clipboard_refused: "This server does not allow the clipboard, so copy and paste stay on each side.",
    desktop_notice_clipboard_latin1: "This server's clipboard only takes Western characters; text with other characters was not sent.",
    desktop_notice_text_too_large: "Clipboard text over 1 MiB was cut to fit.",
    desktop_notice_image_too_large: "A clipboard image over 32 MiB was not passed on.",
    desktop_notice_drive_unavailable: "The local folder was not mapped ({reason}); the session goes on without it.",
    desktop_notice_drive_refused: "The server did not accept the mapped folder; the session goes on without it.",
    desktop_drive_missing: "it does not exist",
    desktop_drive_not_a_folder: "the path is a file",
    desktop_drive_not_absolute: "it is not a full path",
    desktop_drive_too_long: "the path is too long",
    desktop_drive_no_access: "it cannot be read",
    desktop_drive_not_added: "the drive could not be set up",
    desktop_drive_other: "the reason is unknown",
    desktop_notice_dismiss: "Dismiss",
    serial_echo: "Echo",
    serial_echo_on: "on",
    serial_echo_off: "off",
    serial_newline: "Newline",
    serial_ports_refresh: "Refresh ports",
    serial_ports_failed: "The port list could not be read",
    serial_ports_empty: "This machine reports no serial ports",
    serial_ports_missing: "The saved device is not attached",
    serial_ports_attached: "Attached",
    serial_ports_hint: "Plug the device in, then refresh the list",
    serial_still_held: "The port is still held by the driver",
    files_open: "Files",
    files_up: "Up",
    files_refresh: "Refresh",
    files_parent_row: "Parent directory",
    files_loading: "Reading the directory…",
    files_empty: "This directory is empty.",
    files_empty_hint: "Drag a file from the other side to copy it here, or use New folder above.",
    files_other_side: "Other side",
    files_unknown_path: "Waiting for the server",
    files_failed: "The directory could not be read",
    files_terminal_note: "The terminal keeps running. Only this source is affected.",
    files_refresh_source: "Refresh reads this source again. The terminal stays connected.",
    files_kind_directory: "Directory",
    files_kind_file: "File",
    files_kind_link: "Link",
    files_kind_other: "Other",
    files_new: "New folder",
    files_rename: "Rename",
    files_delete: "Delete",
    files_new_placeholder: "Folder name",
    files_rename_placeholder: "New name",
    files_create: "Create",
    files_rename_confirm: "Rename",
    files_delete_confirm: "Delete",
    files_cancel: "Cancel",
    files_target_label: "Target",
    files_kind_label: "Type",
    files_delete_recursive: "This directory and everything inside it are deleted on the server. This cannot be undone.",
    files_delete_file: "This file is deleted on the server. This cannot be undone.",
    files_change_pending: "Waiting for the server\u{2026}",
    files_change_done: "The server applied this change",
    files_change_failed: "The server did not apply every part of this change",
    files_change_changed: "changed",
    files_problem_no_directory: "Open a directory before changing it.",
    files_problem_empty: "A name cannot be empty.",
    files_problem_navigation: "The names . and .. are navigation, not names.",
    files_problem_separator: "A name cannot contain a separator; create one directory at a time.",
    files_problem_taken: "An entry with that name already exists here.",
    files_problem_missing: "That entry is no longer in this directory.",
    files_problem_unchanged: "The name did not change.",
    files_show_remote: "Show remote",
    files_follow_no_report: "The shell has not reported a directory yet. It follows the next change, or browse by hand.",
    files_follow_manual: "Following is off because you browsed by hand. Turn it on to return to the terminal's directory.",
    files_follow_unreadable: "The shell reported a directory this server cannot read; showing the last readable one:",
    files_follow_on: "Follow terminal: on",
    files_follow_off: "Follow terminal: off",
    files_to_remote: "Upload to remote",
    files_to_local: "Download to local",
    files_delete_recursive_local: "This directory and everything inside it are deleted on this computer. This cannot be undone.",
    files_delete_file_local: "This file is deleted on this computer. This cannot be undone.",
    files_change_pending_local: "Waiting for this computer\u{2026}",
    files_change_done_local: "This computer applied this change",
    files_change_failed_local: "This computer did not apply every part of this change",
    files_local_not_found: "This computer has no such path:",
    files_local_denied: "This computer refused access to",
    files_local_not_directory: "This path is not a directory:",
    files_local_lost: "This computer could not be read:",
    files_local_refused: "This computer refused the request:",
    files_local_exists: "That name is already on this computer:",
    files_local_unavailable: "This folder cannot be listed:",
    files_local_root: "This computer",
    files_sort_asc: "Sorted by name, A to Z; press to reverse",
    files_sort_desc: "Sorted by name, Z to A; press to reverse",
    files_sort_size_asc: "Sorted by size, smallest first; press to reverse",
    files_sort_size_desc: "Sorted by size, largest first; press to reverse",
    files_sort_modified_new: "Sorted by modified time, newest first; press to reverse",
    files_sort_modified_old: "Sorted by modified time, oldest first; press to reverse",
    files_filter_placeholder: "Filter names",
    files_filter_count: "{shown} of {total} names",
    files_item_count: "{total} items",
    files_filter_clear: "Clear filter",
    files_filter_none: "No name in this folder matches the filter.",
    files_marked_count: "{count} selected",
    files_mark_all: "Select all",
    files_clear_marks: "Clear selection",
    files_upload_selected: "Upload {count}",
    files_download_selected: "Download {count}",
    files_delete_selected: "Delete {count}",
    files_delete_marked: "Delete the selection",
    files_copy_selected: "Copy {count} paths",
    files_marked_hint: "Ctrl+click adds a row to the selection; Shift+click marks a range; Space marks the row the keyboard is on.",
    files_delete_many_note: "{files} file(s) and {folders} folder(s). A folder is deleted with everything inside it.",
    files_delete_many_target: "Delete {count} entries in",
    files_mode_dock: "Show terminal beside files",
    files_mode_focus: "Focus on files",
    files_uploaded_selected: "Uploading {count} file(s)",
    files_downloaded_selected: "Downloading {count} file(s)",
    files_batch_folders_skipped: "{count} folder(s) were not started: a batch copies files one at a time, and a folder is copied only when the recursive copy is chosen for it.",
    transfers_heading: "Transfers",
    transfers_queued_note: "Queued at the connection; no byte has moved yet.",
    transfers_remaining: "{time} left",
    transfers_empty: "Nothing is being moved for this session.",
    transfers_upload: "Upload",
    transfers_upload_confirm: "Upload",
    transfers_upload_placeholder: "Local file, for example C:\\work\\report.txt",
    transfers_download: "Download",
    transfers_download_confirm: "Download",
    transfers_download_placeholder: "Local folder",
    transfers_cancel: "Cancel",
    transfers_retry: "Retry",
    transfers_residue: "Partial file left behind",
    transfers_overwrite_question: "This target already exists",
    transfers_overwrite_replace: "Replace it",
    transfers_overwrite_keep: "Keep the existing one",
    transfers_problem_empty_path: "Enter a path.",
    transfers_problem_no_name: "The path must end with a file name.",
    transfers_problem_no_source: "The source names no file.",
    transfers_problem_no_directory: "Open a directory first.",
    transfers_problem_no_connection: "The connection cannot take a transfer right now.",
    transfers_problem_needs_recursion: "The source is a directory: choose the recursive copy first.",
    transfers_problem_directory_needs_scp: "The source is a directory: only traditional SCP copies a directory - choose SCP, then the recursive copy.",
    transfers_protocol: "Protocol",
    transfers_protocol_sftp: "SFTP",
    transfers_protocol_scp: "SCP",
    transfers_protocol_note: "SCP copies one file or, when chosen, one directory with the server's own scp program. It has no resume and no directory listing.",
    transfers_recursive: "Copy the directory and everything in it",
    transfers_remote_directory: "Remote directory",
    transfers_remote_path: "Remote path",
    transfers_lands_at: "Saves to {path}",
    files_sftp_scp_hint: "Listing and managing a directory need SFTP. Copying a file still works with SCP: open Upload and type a remote directory (for example ~), or open Download and type the remote file (for example ~/report.txt), then choose SCP.",
    transfers_close_question: "Closing this session cancels its transfers",
    transfers_close_cancel: "Keep the session open",
    transfers_close_confirm: "Close and cancel",
    transfers_resume: "Resume",
    transfers_restart: "Transfer again",
    transfers_resume_from: "can resume from {size}",
    transfers_reveal: "Show in folder",
    transfers_remove: "Remove from list",
    transfers_resume_source: "The source changed, so the partial file was not resumed.",
    transfers_resume_unverifiable: "The partial file could not be checked, so it was not resumed.",
    transfers_resume_unsupported: "This target cannot resume, so the transfer did not continue.",
    transfers_resume_damaged: "The partial file is damaged, so it was not resumed.",
    transfers_close_app_question: "Closing the application stops these transfers",
    transfers_close_app_cancel: "Keep working",
    transfers_close_app_confirm: "Close and stop them",
    transfers_host: "Host",
    palette_placeholder: "Search sessions, shells, hosts and actions",
    palette_empty: "Nothing matches",
    palette_group_sessions: "Open sessions",
    palette_group_shells: "New local shell",
    palette_group_hosts: "Connect to host",
    palette_group_navigate: "Go to",
    palette_group_actions: "Actions",
    palette_group_snippets: "Shortcut commands",
    palette_close_tab: "Close current session",
    palette_language: "Switch interface language",
    palette_text_size: "Change text size",
    palette_restore_tab_title: "Use the automatic tab title",
    palette_clear_tab_accent: "Clear the tab colour",
    palette_current: "Current",
    palette_key_move: "Select",
    palette_key_run: "Open",
    palette_key_close: "Close",
    tab_menu_duplicate: "Duplicate tab",
    tab_menu_close: "Close tab",
    tab_menu_close_others: "Close other tabs",
    tab_menu_close_right: "Close tabs to the right",
    session_menu_open: "Open",
    session_menu_reopen: "Reopen",
    session_menu_close: "Close",
    session_menu_remove: "Remove",
    session_menu_remove_closed: "Remove all closed",
    session_more: "More actions",
    session_closed_summary: "Closed sessions",
    field_cut: "Cut",
    field_copy: "Copy",
    field_paste: "Paste",
    field_select_all: "Select all",
    field_reveal: "Show",
    field_conceal: "Hide",
    field_browse: "Browse…",
    host_auth_title: "Sign in with",
    host_secret_passphrase: "Key passphrase",
    host_remember_password: "Remember password",
    host_remember_passphrase: "Remember passphrase",
    host_remember_note: "Kept in the system credential store (Keychain or Credential Manager), never in the host file.",
    host_secret_saved_placeholder: "Saved; leave empty to keep it",
    host_secret_failed: "The password was not saved to the system credential store",
    host_startup_label: "Run after connecting",
    host_startup_placeholder: "For example cd /srv && tmux attach",
    host_startup_note: "Typed into the remote shell once, each time the connection opens.",
    x11_label: "X11 forwarding",
    x11_off_label: "Off",
    x11_restricted_label: "Restricted (-X)",
    x11_trusted_label: "Trusted (-Y)",
    x11_note: "Off by default. Restricted uses the X11 SECURITY extension, as ssh -X does, and needs xauth on this computer to get an untrusted cookie plus a display server that enforces that extension; a display server that does not is reported with its own reason and next step. Trusted removes those limits, as ssh -Y does. A display server must already be running on this computer.",
    x11_connect_label: "X11 forwarding for this connection",
    x11_trusted_title: "Allow trusted X11 forwarding?",
    x11_trusted_body: "Trusted forwarding lets remote programs use this computer's display without the X11 SECURITY extension. A program on the server can then read what you type in other windows of that display, including windows of other applications. The choice applies to this connection only.",
    x11_trusted_confirm: "Use trusted forwarding",
    x11_trusted_restricted: "Use restricted forwarding",
    x11_trusted_cancel: "Do not connect",
    x11_bar_off: "X11 off",
    x11_bar_enabling: "X11 starting",
    x11_bar_ready: "X11",
    x11_bar_failed: "X11 failed",
    host_save_connect: "Save and connect",
    remote_saved_secret_failed: "The saved password could not be read",
    theme_system: "System",
    theme_light: "Light",
    theme_dark: "Dark",
    palette_theme: "Change theme",
    motion_full: "Full",
    motion_reduced: "Reduced",
    palette_motion: "Reduce motion",
    quick_connect: "Quick connect",
    quick_connect_hint: "Connect now. The host is saved only if you ask.",
    quick_connect_title: "Quick connect",
    quick_save_host: "Save as host",
    quick_saved: "Saved as a host",
    quick_unsaved_note: "This connection is not a saved host yet",
    attempt_others: "Other connections",
    attempt_cancel: "Cancel",
    palette_group_recent: "Recent connections",
    quick_port_invalid: "The port is not a whole number from 1 to 65535",
    quick_port_blank: "empty",
    quick_fix_port: "Change the Port field.",
    quick_invalid_name: "The address is empty, so this connection has no name. Change the Address field.",
    quick_invalid_address: "The address is empty. Change the Address field.",
    quick_invalid_address_space: "The address contains a space. Change the Address field so it has none.",
    quick_invalid_user: "The user name is empty. Change the User field.",
    quick_saved_next: "Open it later from the host list. This session stays connected.",
    quick_save_write_failed: "The host file could not be written, so this connection was not saved.",
    quick_save_write_next: "This session stays connected. Make the host file writable, then save again.",
    quick_save_duplicate: "A host with this identifier is already saved, so nothing was written.",
    quick_save_duplicate_next: "Open that saved host to change it, or leave this connection unsaved.",
    quick_save_unknown: "That host is not in the saved file, so it could not be updated.",
    quick_save_unknown_next: "Save it as a new host, or check the host file. This session stays connected.",
    quick_save_inconsistent: "The saved hosts disagree with each other, so this one was not written.",
    quick_save_inconsistent_next: "Repair the host file, then save again. This session stays connected.",
    host_jump_title: "Jump hosts",
    host_jump_empty: "Direct connection. Add a saved host to reach this one through it.",
    host_jump_add: "Add jump host",
    host_jump_up: "Move up",
    host_jump_down: "Move down",
    host_jump_remove: "Remove",
    host_proxy_label: "Proxy command",
    host_proxy_placeholder: "For example nc -X connect -x proxy:1080 %h %p",
    host_proxy_note: "Stored as text. It runs only after you confirm the expanded command.",
    host_proxy_conflict: "This host has both a jump chain and a ProxyCommand. Keep one before connecting.",
    remote_asking_hop: "Sign in to",
    remote_proxy_title: "Confirm ProxyCommand",
    remote_proxy_confirm: "Run command",
    remote_proxy_refuse: "Do not run",
    import_proxy_label: "Proxy command",
    import_jump_label: "Jump",
    import_forward_label: "Forward",
    import_forwards_failed: "Hosts were saved, but the forwarding rules were not written.",
    pane_split_right: "Split right",
    pane_split_down: "Split down",
    pane_zoom: "Zoom pane",
    pane_unzoom: "Show all panes",
    pane_active: "Active",
    pane_label: "Pane",
    pane_pending: "Not connected",
    pane_pending_body: "This pane is waiting. Nothing was started, and earlier output is not shown.",
    pane_connect: "Connect",
    pane_connect_all: "Start all panes",
    pane_remove: "Remove pane",
    pane_close: "Close pane",
    pane_resize_less: "Shrink pane",
    pane_resize_more: "Grow pane",
    tab_menu_move_left: "Move tab left",
    tab_menu_move_right: "Move tab right",
    tab_menu_rename: "Rename tab\u{2026}",
    tab_rename_title: "Rename tab",
    tab_rename_placeholder: "Tab name",
    tab_rename_hint: "The name replaces the automatic title. The target, the kind and the state stay in the tooltip and in the tab list.",
    tab_rename_save: "Save name",
    tab_rename_empty: "A tab needs a name here. Choose \u{201c}Use automatic title\u{201d} to clear it.",
    tab_rename_auto: "Use automatic title",
    tab_rename_limit: "A tab name holds {max} characters. Saving keeps the first {max} and drops the rest.",
    tab_rename_shortened: "Saved the first {max} characters. Edit the name and save again to change it.",
    tab_accent_label: "Tab colour",
    tab_accent_none: "No colour",
    tab_accent_iris: "Iris",
    tab_accent_violet: "Violet",
    tab_accent_teal: "Teal",
    tab_accent_lime: "Lime",
    tab_accent_orchid: "Orchid",
    tab_accent_clay: "Clay",
    tab_list_title: "All tabs",
    tabs_scroll_start: "Scroll tabs left",
    tabs_scroll_end: "Scroll tabs right",
    tab_list_hint: "Every open tab, in strip order. Choosing one shows it.",
    tab_mark_bell: "Bell",
    tab_mark_finished: "Ended",
    tab_mark_bell_hint: "A background shell rang the bell. Open this tab to clear the mark; the bell byte still reached its program.",
    tab_mark_finished_hint: "The shell reported that a background command ended. Open this tab to clear the mark.",
    pane_missing_host: "This host is not saved",
    pane_missing_next: "Fix the host or remove the pane. Nothing was connected.",
    workspace_dangling_tab: "The saved active tab is not in this workspace. Choose a tab. The others were kept.",
    workspace_dangling_pane: "The saved active pane is not on its tab. Choose a pane. The layout was kept.",
    pane_mode_classic: "Classic terminal",
    pane_mode_integrated: "Command blocks, not restored",
    terminal_mode_blocks: "Command blocks",
    terminal_block_running: "Running",
    terminal_block_unknown: "Unknown",
    terminal_block_exit: "Exit",
    terminal_block_copy: "Copy block",
    terminal_block_rerun: "Run again",
    terminal_search: "Search this terminal",
    terminal_search_empty: "No matches in this terminal",
    terminal_search_invalid: "Invalid regular expression",
    terminal_search_next: "Next match",
    terminal_search_previous: "Previous match",
    terminal_search_case: "Match case",
    terminal_search_regex: "Regular expression",
    terminal_search_close: "Close search",
    terminal_search_keys: "Enter next · Esc close",
    terminal_search_keyboard_terminal: "Keys go to the terminal",
    terminal_paste_title: "Confirm paste",
    terminal_paste_target: "Target session",
    terminal_paste_confirm: "Paste into this session",
    terminal_paste_cancel: "Do not paste",
    terminal_paste_lines: "lines",
    terminal_paste_runs: "Runs once you confirm",
    terminal_paste_run_confirm: "Run this command",
    terminal_paste_safe: "Esc or Do not paste writes nothing and keeps the keyboard in the terminal.",
    terminal_no_blocks: "This shell has not reported command blocks. The grid is unchanged.",
    terminal_blocks_failed: "Shell integration failed. The classic grid was kept.",
    terminal_filter_all: "All commands",
    terminal_filter_running: "Running only",
    terminal_filter_unknown: "Unknown exit",
    terminal_filter_failed: "Non-zero exit",
    terminal_filter_zero: "Exit 0 only",
    nav_forwards: "Forwards",
    nav_forwards_summary: "Local, remote and dynamic tunnels",
    empty_forwards_title: "No forwarding rules",
    empty_forwards_body: "Saved rules stay here until you start one. Opening this page does not listen.",
    forward_saved: "Saved",
    forward_starting: "Starting",
    forward_listening: "Listening",
    forward_stopped: "Stopped",
    forward_failed: "Not listening",
    forward_start: "Start",
    forward_stop: "Stop",
    forward_save: "Save rule",
    forward_delete: "Delete",
    forward_edit: "Edit",
    forward_exposure_title: "This listen address is not loopback",
    forward_exposure_all: "Binding every interface lets other machines that can reach this computer connect. The rule is not started until you confirm.",
    forward_exposure_named: "This address is not loopback. Machines that can reach it can connect. The rule is not started until you confirm.",
    forward_exposure_confirm: "Listen on this address",
    forward_exposure_cancel: "Keep it saved",
    forward_close_rules: "Closing this connection stops only these forwards",
    forward_close_app: "Closing the application stops these forwards",
    forward_open: "Forwards",
    forward_carrier_lost: "The connection closed, so this forward is no longer listening.",
    forward_connect_only: "Connect without a terminal",
};

/// Chinese copy.
pub static CHINESE: Strings = Strings {
    nav_local: "终端",
    nav_local_summary: "已打开的本地与远程终端",
    nav_hosts: "主机",
    nav_hosts_summary: "已保存的服务器与 SSH 设置",
    nav_sessions: "会话",
    nav_sessions_summary: "所有已打开的终端",
    nav_transfers: "传输",
    nav_transfers_summary: "上传与下载",
    empty_local_title: "还没有打开本地终端",
    empty_local_body: "本地终端在当前机器的当前目录运行，不需要网络。",
    empty_hosts_title: "还没有保存的主机",
    empty_hosts_body: "保存主机后会记住地址、端口和用户名，下次连接不必重新输入。",
    empty_sessions_title: "还没有会话",
    empty_sessions_body: "本地与远程终端会各自显示名称、类型和当前状态。",
    empty_transfers_title: "还没有传输任务",
    empty_transfers_body: "上传与下载会在这里显示源、目标和进度。",
    loading_title: "正在等待真实数据",
    loading_body: "在系统返回实际结果之前，这里不会显示任何内容。",
    failed_title: "这一步没能完成",
    recovery_label: "建议的下一步",
    recovery_retry: "重试",
    recovery_create_host: "新建主机",
    recovery_import_config: "导入 SSH 配置",
    recovery_choose_shell: "选择 Shell",
    next_step: "下一步",
    primary_hosts: "新建主机",
    unavailable_notice: "当前版本没有实现这个操作，因此没有启动任何任务。",
    count_hosts: "台主机",
    count_host_one: "台主机",
    local_shell_fallback: "系统 Shell",
    shell_custom_button: "自定义路径",
    shell_custom_placeholder: "输入程序名或完整路径后按回车",
    shell_none_found: "本机没有找到可用的 Shell。请输入程序路径来启动一个。",
    terminal_live: "运行中",
    terminal_failed: "启动失败",
    terminal_exited: "已退出",
    terminal_terminated: "被终止",
    terminal_disconnected: "连接已断开",
    session_details_show: "展开诊断",
    session_details_hide: "收起诊断",
    terminal_environment_unavailable: "未能读取登录环境：这个 Shell 使用应用继承的环境启动",
    terminal_environment_unreadable: "登录 Shell 没有返回可读的环境：这个 Shell 使用应用继承的环境启动",
    terminal_environment_locale_replaced: "当前 locale 不是 UTF-8：这个 Shell 改用 UTF-8 启动",
    terminal_environment_locale_added: "未继承任何 locale：这个 Shell 使用 UTF-8 启动",
    terminal_copy: "复制",
    terminal_smaller: "缩小字号",
    terminal_larger: "放大字号",
    shell_custom_hint: "本机上的任意程序",
    keys_search_placeholder: "搜索动作或快捷键",
    hosts_view_all: "全部主机",
    hosts_view_recent: "最近连接",
    hosts_recent_empty: "还没有连接记录。主机真正连接成功之后才会出现在这里。",
    hosts_no_match: "没有符合这个搜索条件的主机。",
    host_field_name: "名称",
    host_field_address: "地址",
    host_field_port: "端口",
    host_field_user: "用户名",
    host_field_group: "分组",
    host_field_identity: "密钥文件",
    host_identity_note: "只保存密钥路径，Shellune 从不读取或复制密钥内容。",
    host_editor_new: "新建主机",
    host_editor_edit: "编辑主机",
    host_save: "保存主机",
    host_cancel: "取消",
    host_edit: "编辑",
    host_delete: "删除",
    host_delete_question: "要删除这台已保存的主机吗？",
    host_delete_confirm: "删除主机",
    host_delete_note: "只删除本地保存的记录，服务器上的内容不会改变。",
    hosts_saved: "主机已保存，没有打开任何连接。",
    hosts_deleted: "主机已删除。",
    hosts_retry: "重新读取文件",
    host_last_connected: "最近连接",
    host_connection_count: "次连接",

    import_open: "导入 SSH 配置",
    import_title: "导入 SSH 配置",
    import_path_label: "配置文件",
    import_path_placeholder: "SSH 配置文件的路径",
    import_preview: "预览",
    import_source_label: "读取自",
    import_read_only: "只读取这个文件，不会写回原配置。",
    import_limits_title: "导入前的限制",
    import_limits_none: "这个文件里的每台主机都已完整读取。",
    import_outcome_ready: "可直接导入",
    import_outcome_review: "需要复核",
    import_outcome_blocked: "已阻止",
    import_outcome_saved: "已保存",
    import_not_offered: "不导入",
    import_ignored_label: "未应用的指令",
    import_include_label: "没有读取被包含的文件",
    import_match_label: "没有评估 Match 条件块",
    import_unmatched_proxy_label: "没有匹配到任何主机的代理或转发规则",
    import_nothing: "这个文件里还没有可以直接导入的主机。",
    import_all_saved: "这个文件里的主机都已保存。",
    import_apply: "导入主机",
    import_applied: "已导入",
    import_review_count: "需复核",
    import_skipped: "已跳过",
    import_error_title: "没有导入任何内容",
    import_path_required: "请先填写要读取的配置文件路径。",
    auth_password: "密码",
    auth_key: "私钥文件",
    auth_key_passphrase: "带口令的私钥",
    auth_agent: "系统 Agent",
    auth_password_hint: "仅用于本次连接，Shellune 不会保存。",
    auth_key_hint: "服务器已授权、且本账号可读取的私钥。",
    auth_key_passphrase_hint: "口令仅用于本次解密该私钥。",
    auth_agent_hint: "使用系统 Agent 中已有的密钥完成签名。",
    auth_missing_password: "请输入该主机的密码。",
    auth_missing_key: "请选择要使用的私钥文件。",
    auth_missing_passphrase: "请输入该私钥的口令。",
    remote_connect: "连接",
    remote_idle: "尚未连接",
    remote_connecting: "正在连接",
    remote_verifying: "等待你确认服务器身份",
    remote_live: "SSH 会话已连接",
    remote_failed: "连接未建立",
    remote_waiting: "等待你操作",
    remote_wait_identity: "服务器身份",
    remote_cancel_attempt: "取消连接",
    trust_title: "尚未信任该服务器身份",
    trust_body: "请与服务器管理员可打印的指纹核对；在你接受之前不会写入任何记录。",
    trust_algorithm: "算法",
    trust_fingerprint: "SHA-256 指纹",
    trust_accept: "接受并连接",
    trust_reject: "拒绝",
    trust_rejected_note: "已拒绝该身份，未打开终端，也未写入信任记录。",
    trust_undelivered_note: "连接尝试已结束，该身份既未被接受也未被记录。请重新连接以再次查看。",
    remote_failure_title: "连接没有建立",
    remote_close: "关闭连接面板",
    session_kind_local: "本地",
    session_kind_remote: "SSH 远程",
    session_kind_desktop: "远程桌面",
    session_kind_serial: "串口",
    connection_editor_new: "新建连接",
    connection_editor_edit: "编辑连接",
    connection_field_protocol: "协议",
    connection_field_domain: "域",
    connection_field_certificate: "证书",
    connection_field_security: "安全",
    connection_field_device: "设备",
    connection_field_baud: "波特率",
    connection_field_data_bits: "数据位",
    connection_field_parity: "校验",
    connection_field_stop_bits: "停止位",
    connection_field_flow: "流控",
    connection_field_local_echo: "本地回显",
    connection_field_clipboard: "共享剪贴板",
    connection_field_drive_folder: "映射本机文件夹",
    connection_drive_folder_placeholder: "留空则不映射",
    connection_drive_folder_note: "远端把这个文件夹看作名为 Shellune 的磁盘，无法访问它以外的文件。",
    connection_not_started_note: "保存连接不会打开任何东西：只有你启动会话时才会拨号或打开设备。",
    connection_copy_suffix: "（副本）",
    connection_delete: "删除连接",
    connection_delete_question: "删除这条已保存的连接？正在运行的内容不受影响。",
    connection_duplicate: "复制一份",
    connection_saved: "连接已保存",
    connection_deleted: "连接已删除",
    session_close: "关闭会话",
    session_reconnect: "重新连接",
    session_reconnect_no_shell: "该会话没有记录可启动的程序，无法重新启动。",
    session_reconnect_missing_host: "该会话使用的主机记录已被删除，无法再次连接。",
    session_reconnect_not_wired: "这类会话暂时还不能启动，所以没有启动任何东西。",
    session_no_terminal: "该会话当前没有终端",
    desktop_live: "远程桌面",
    desktop_ended: "远程桌面会话已结束",
    desktop_credentials_title: "登录远程桌面",
    desktop_certificate_remember: "记住这台主机的证书",
    desktop_certificate_changed: "服务器证书已变更。上次信任的指纹",
    desktop_certificate_title: "核对服务器证书",
    desktop_certificate_subject: "颁发给",
    desktop_certificate_issuer: "颁发者",
    desktop_certificate_trust: "信任并连接",
    desktop_certificate_reject: "拒绝",
    desktop_stage_connecting: "正在建立连接",
    desktop_stage_certificate: "等待证书确认",
    desktop_stage_signing_in: "正在认证",
    desktop_stage_establishing: "正在等待首屏画面",
    desktop_wait_time: "已等待",
    desktop_hide_controls: "收起桌面工具栏",
    desktop_show_controls: "展开桌面工具栏",
    desktop_fullscreen_enter: "全屏",
    desktop_fullscreen_exit: "退出全屏",
    desktop_zoom_fit: "适应窗口",
    desktop_zoom_in: "放大",
    desktop_zoom_out: "缩小",
    desktop_notice_clipboard_refused: "该服务器不允许剪贴板，复制粘贴不会在本机与远端之间同步。",
    desktop_notice_clipboard_latin1: "该服务器的剪贴板只支持西文字符，含其他字符的文本没有发送。",
    desktop_notice_text_too_large: "剪贴板文本超过 1 MiB，已截断。",
    desktop_notice_image_too_large: "剪贴板图片超过 32 MiB，没有传输。",
    desktop_notice_drive_unavailable: "本机文件夹没有映射（{reason}），会话照常进行。",
    desktop_notice_drive_refused: "服务器没有接受映射的文件夹，会话照常进行。",
    desktop_drive_missing: "文件夹不存在",
    desktop_drive_not_a_folder: "该路径是文件",
    desktop_drive_not_absolute: "不是完整路径",
    desktop_drive_too_long: "路径过长",
    desktop_drive_no_access: "没有读取权限",
    desktop_drive_not_added: "无法建立磁盘",
    desktop_drive_other: "原因未知",
    desktop_notice_dismiss: "知道了",
    serial_echo: "本地回显",
    serial_echo_on: "开",
    serial_echo_off: "关",
    serial_newline: "换行",
    serial_ports_refresh: "刷新串口",
    serial_ports_failed: "无法读取串口列表",
    serial_ports_empty: "本机没有可用的串口",
    serial_ports_missing: "已保存的设备当前未接入",
    serial_ports_attached: "已接入",
    serial_ports_hint: "接入设备后刷新列表",
    serial_still_held: "端口仍被驱动占用",
    files_open: "文件",
    files_up: "上级",
    files_refresh: "刷新",
    files_parent_row: "上级目录",
    files_loading: "正在读取目录…",
    files_empty: "这个目录是空的。",
    files_empty_hint: "从另一侧把文件拖过来即可复制到这里，或使用上面的新建文件夹。",
    files_other_side: "另一侧",
    files_unknown_path: "等待服务器返回",
    files_failed: "无法读取这个目录",
    files_terminal_note: "终端仍在运行，只有这个来源受影响。",
    files_refresh_source: "刷新会重新读取这个来源，终端保持连接。",
    files_kind_directory: "目录",
    files_kind_file: "文件",
    files_kind_link: "链接",
    files_kind_other: "其他",
    files_new: "新建文件夹",
    files_rename: "重命名",
    files_delete: "删除",
    files_new_placeholder: "文件夹名称",
    files_rename_placeholder: "新名称",
    files_create: "创建",
    files_rename_confirm: "重命名",
    files_delete_confirm: "删除",
    files_cancel: "取消",
    files_target_label: "目标",
    files_kind_label: "类型",
    files_delete_recursive: "这个目录及其中的全部内容都会在服务器上删除，且无法撤销。",
    files_delete_file: "这个文件会在服务器上删除，且无法撤销。",
    files_change_pending: "等待服务器\u{2026}",
    files_change_done: "服务器已应用这次修改",
    files_change_failed: "服务器没有完成这次修改的全部内容",
    files_change_changed: "已修改",
    files_problem_no_directory: "请先打开一个目录再修改。",
    files_problem_empty: "名称不能为空。",
    files_problem_navigation: "“.” 和 “..” 是导航，不是名称。",
    files_problem_separator: "名称不能包含分隔符；请逐级创建目录。",
    files_problem_taken: "这里已经有同名条目。",
    files_problem_missing: "这个条目已不在当前目录中。",
    files_problem_unchanged: "名称没有变化。",
    files_show_remote: "显示远程",
    files_follow_no_report: "终端还没有报告目录。切换一次目录后会自动跟随，也可以手动浏览。",
    files_follow_manual: "已按你的浏览手动选择目录，跟随已关闭。重新打开可回到终端目录。",
    files_follow_unreadable: "终端报告的目录无法读取，仍在显示上一个可读目录：",
    files_follow_on: "跟随终端目录：开",
    files_follow_off: "跟随终端目录：关",
    files_to_remote: "上传到远程",
    files_to_local: "下载到本地",
    files_delete_recursive_local: "这个目录及其中的全部内容都会从这台电脑删除，且无法撤销。",
    files_delete_file_local: "这个文件会从这台电脑删除，且无法撤销。",
    files_change_pending_local: "正在等待这台电脑\u{2026}",
    files_change_done_local: "这台电脑已应用这次修改",
    files_change_failed_local: "这台电脑没有完成这次修改的全部内容",
    files_local_not_found: "这台电脑上没有这个路径：",
    files_local_denied: "这台电脑拒绝访问",
    files_local_not_directory: "这个路径不是目录：",
    files_local_lost: "无法读取这台电脑：",
    files_local_refused: "这台电脑拒绝了请求：",
    files_local_exists: "这台电脑上已经有这个名称：",
    files_local_unavailable: "无法列出这个文件夹：",
    files_local_root: "这台电脑",
    files_sort_asc: "按名称排序，A→Z；再次点击反向",
    files_sort_desc: "按名称排序，Z→A；再次点击反向",
    files_sort_size_asc: "按大小排序，从小到大；再次点击反向",
    files_sort_size_desc: "按大小排序，从大到小；再次点击反向",
    files_sort_modified_new: "按修改时间排序，最新的在前；再次点击反向",
    files_sort_modified_old: "按修改时间排序，最早的在前；再次点击反向",
    files_filter_placeholder: "筛选名称",
    files_filter_count: "{shown}/{total} 项",
    files_item_count: "{total} 项",
    files_filter_clear: "清除筛选",
    files_filter_none: "该目录没有名称符合筛选条件。",
    files_marked_count: "已选 {count} 项",
    files_mark_all: "全选",
    files_clear_marks: "清除选择",
    files_upload_selected: "上传 {count} 项",
    files_download_selected: "下载 {count} 项",
    files_delete_selected: "删除 {count} 项",
    files_delete_marked: "删除所选项目",
    files_copy_selected: "复制 {count} 条路径",
    files_marked_hint: "Ctrl+点击加入选择，Shift+点击选中连续范围，空格键选中光标所在行。",
    files_delete_many_note: "{files} 个文件，{folders} 个文件夹。文件夹会连同其中内容一起删除。",
    files_delete_many_target: "删除以下 {count} 项，位置",
    files_mode_dock: "同屏显示终端与文件",
    files_mode_focus: "专注文件",
    files_uploaded_selected: "正在上传 {count} 个文件",
    files_downloaded_selected: "正在下载 {count} 个文件",
    files_batch_folders_skipped: "{count} 个文件夹未启动：批量传输一次复制一个文件，文件夹只有在为它勾选递归复制时才会复制。",
    transfers_heading: "传输",
    transfers_queued_note: "已交给连接排队，还没有开始传字节。",
    transfers_remaining: "剩余 {time}",
    transfers_empty: "这个会话当前没有传输。",
    transfers_upload: "上传",
    transfers_upload_confirm: "上传",
    transfers_upload_placeholder: "本地文件，例如 C:\\work\\报告.txt",
    transfers_download: "下载",
    transfers_download_confirm: "下载",
    transfers_download_placeholder: "本地目录",
    transfers_cancel: "取消",
    transfers_retry: "重试",
    transfers_residue: "残留的部分文件",
    transfers_overwrite_question: "目标已经存在",
    transfers_overwrite_replace: "覆盖它",
    transfers_overwrite_keep: "保留原文件",
    transfers_problem_empty_path: "请输入路径。",
    transfers_problem_no_name: "路径必须以文件名结尾。",
    transfers_problem_no_source: "源路径没有文件名。",
    transfers_problem_no_directory: "请先打开一个目录。",
    transfers_problem_no_connection: "连接当前无法接受传输。",
    transfers_problem_needs_recursion: "源是一个目录：请先选择递归复制。",
    transfers_problem_directory_needs_scp: "源是一个目录：只有传统 SCP 能复制目录，请把协议改为 SCP 再勾选递归复制。",
    transfers_protocol: "协议",
    transfers_protocol_sftp: "SFTP",
    transfers_protocol_scp: "SCP",
    transfers_protocol_note: "SCP 用服务器自带的 scp 程序复制一个文件，或在你选择时复制整个目录；它不支持续传，也不提供目录列表。",
    transfers_recursive: "复制整个目录及其内容",
    transfers_remote_directory: "远端目录",
    transfers_remote_path: "远端路径",
    transfers_lands_at: "将保存到 {path}",
    files_sftp_scp_hint: "目录列表与管理需要 SFTP。复制文件仍可用 SCP：打开“上传”并填写远端目录（例如 ~），或打开“下载”并填写远端文件（例如 ~/报告.txt），再选择 SCP。",
    transfers_close_question: "关闭这个会话会取消它的传输",
    transfers_close_cancel: "保持会话打开",
    transfers_close_confirm: "关闭并取消",
    transfers_resume: "续传",
    transfers_restart: "重新传输",
    transfers_resume_from: "可从 {size} 处续传",
    transfers_reveal: "在文件夹中显示",
    transfers_remove: "从列表移除",
    transfers_resume_source: "源已变化，没有从部分文件续传。",
    transfers_resume_unverifiable: "无法校验部分文件，因此没有续传。",
    transfers_resume_unsupported: "目标不支持续传，因此没有继续。",
    transfers_resume_damaged: "部分文件已损坏，因此没有续传。",
    transfers_close_app_question: "关闭应用会中断这些传输",
    transfers_close_app_cancel: "继续工作",
    transfers_close_app_confirm: "关闭并停止",
    transfers_host: "主机",
    palette_placeholder: "搜索会话、Shell、主机与操作",
    palette_empty: "没有匹配的结果",
    palette_group_sessions: "已打开的会话",
    palette_group_shells: "新建本地终端",
    palette_group_hosts: "连接到主机",
    palette_group_navigate: "前往",
    palette_group_actions: "操作",
    palette_group_snippets: "快捷命令",
    palette_close_tab: "关闭当前会话",
    palette_language: "切换界面语言",
    palette_text_size: "调整文字大小",
    palette_restore_tab_title: "恢复标签自动标题",
    palette_clear_tab_accent: "不设标签辨识色",
    palette_current: "当前",
    palette_key_move: "选择",
    palette_key_run: "打开",
    palette_key_close: "关闭",
    tab_menu_duplicate: "复制标签",
    tab_menu_close: "关闭标签",
    tab_menu_close_others: "关闭其他标签",
    tab_menu_close_right: "关闭右侧标签",
    session_menu_open: "打开",
    session_menu_reopen: "重新打开",
    session_menu_close: "关闭",
    session_menu_remove: "删除",
    session_menu_remove_closed: "清除所有已关闭",
    session_more: "更多操作",
    session_closed_summary: "已关闭的会话",
    field_cut: "剪切",
    field_copy: "复制",
    field_paste: "粘贴",
    field_select_all: "全选",
    field_reveal: "显示",
    field_conceal: "隐藏",
    field_browse: "浏览…",
    host_auth_title: "登录方式",
    host_secret_passphrase: "私钥口令",
    host_remember_password: "记住密码",
    host_remember_passphrase: "记住口令",
    host_remember_note: "保存在系统凭据库（钥匙串 / 凭据管理器），不会写入主机文件。",
    host_secret_saved_placeholder: "已保存；留空则保持不变",
    host_secret_failed: "密码没有保存到系统凭据库",
    host_startup_label: "连接后运行",
    host_startup_placeholder: "例如 cd /srv && tmux attach",
    host_startup_note: "每次连接成功后，在远程 Shell 中自动执行一次。",
    x11_label: "X11 转发",
    x11_off_label: "关闭",
    x11_restricted_label: "受限（-X）",
    x11_trusted_label: "信任（-Y）",
    x11_note: "默认关闭。受限模式与 ssh -X 一样交给 X11 SECURITY 扩展限制远程程序，需要本机有 xauth 才能取得不受信任的 cookie，并且需要显示服务真的执行这个扩展（不执行的显示服务会给出独立的原因与下一步）；信任模式与 ssh -Y 一样去掉这些限制。这台电脑上必须已经运行显示服务。",
    x11_connect_label: "本次连接的 X11 转发",
    x11_trusted_title: "允许信任模式的 X11 转发？",
    x11_trusted_body: "信任模式会让远程程序不受 X11 SECURITY 扩展限制地使用这台电脑的显示服务。服务器上的程序因此可能读取你在该显示其它窗口中输入的内容，包括其它应用的窗口。该选择只对本次连接生效。",
    x11_trusted_confirm: "使用信任模式",
    x11_trusted_restricted: "改用受限模式",
    x11_trusted_cancel: "不连接",
    x11_bar_off: "X11 关",
    x11_bar_enabling: "X11 启用中",
    x11_bar_ready: "X11",
    x11_bar_failed: "X11 失败",
    host_save_connect: "保存并连接",
    remote_saved_secret_failed: "无法读取已保存的密码",
    theme_system: "跟随系统",
    theme_light: "浅色",
    theme_dark: "深色",
    palette_theme: "切换主题",
    motion_full: "完整",
    motion_reduced: "减少",
    palette_motion: "减少动态效果",
    quick_connect: "快速连接",
    quick_connect_hint: "先连接，只有你要求时才保存为主机。",
    quick_connect_title: "快速连接",
    quick_save_host: "保存为主机",
    quick_saved: "已保存为主机",
    quick_unsaved_note: "这次连接还没有保存为主机",
    attempt_others: "其他连接",
    attempt_cancel: "取消",
    palette_group_recent: "最近连接",
    quick_port_invalid: "端口必须是 1 到 65535 的整数",
    quick_port_blank: "空的",
    quick_fix_port: "请修改端口这一栏。",
    quick_invalid_name: "地址是空的，这次连接没有名称。请修改地址这一栏。",
    quick_invalid_address: "地址是空的。请修改地址这一栏。",
    quick_invalid_address_space: "地址里含有空格。请修改地址这一栏，去掉其中的空格。",
    quick_invalid_user: "用户名是空的。请修改用户这一栏。",
    quick_saved_next: "之后可以从主机列表再次打开。当前会话保持连接。",
    quick_save_write_failed: "主机文件没有写入，所以这次连接没有保存成主机。",
    quick_save_write_next: "当前会话保持连接。让主机文件可以写入后，再保存一次。",
    quick_save_duplicate: "已经有一台主机使用了这个标识，所以没有再写入。",
    quick_save_duplicate_next: "打开已保存的那台主机再修改，或者让这次连接保持未保存。",
    quick_save_unknown: "主机文件里没有这一台，所以无法更新它。",
    quick_save_unknown_next: "可以把它另存为一台新主机，或先检查主机文件。当前会话保持连接。",
    quick_save_inconsistent: "已保存的主机记录互相不一致，所以这一台没有写入。",
    quick_save_inconsistent_next: "先修好主机文件，再保存一次。当前会话保持连接。",
    host_jump_title: "跳板主机",
    host_jump_empty: "直接连接。添加已保存的主机，即可经它到达这一台。",
    host_jump_add: "添加跳板",
    host_jump_up: "上移",
    host_jump_down: "下移",
    host_jump_remove: "移除",
    host_proxy_label: "代理命令",
    host_proxy_placeholder: "例如 nc -X connect -x proxy:1080 %h %p",
    host_proxy_note: "只保存命令文本。展开后的命令需要你确认才会执行。",
    host_proxy_conflict: "这台主机同时有跳板链和 ProxyCommand。连接前只保留一种。",
    remote_asking_hop: "登录到",
    remote_proxy_title: "确认代理命令",
    remote_proxy_confirm: "运行命令",
    remote_proxy_refuse: "不运行",
    import_proxy_label: "代理命令",
    import_jump_label: "跳板",
    import_forward_label: "转发",
    import_forwards_failed: "主机已保存，但转发规则没有写入。",
    pane_split_right: "向右拆分",
    pane_split_down: "向下拆分",
    pane_zoom: "放大窗格",
    pane_unzoom: "显示全部分屏",
    pane_active: "活动",
    pane_label: "窗格",
    pane_pending: "待连接",
    pane_pending_body: "这个窗格在等待。没有启动任何进程，之前的输出也不会显示。",
    pane_connect: "连接",
    pane_connect_all: "全部启动",
    pane_remove: "移除窗格",
    pane_close: "关闭窗格",
    pane_resize_less: "缩小窗格",
    pane_resize_more: "放大尺寸",
    tab_menu_move_left: "标签左移",
    tab_menu_move_right: "标签右移",
    tab_menu_rename: "重命名标签\u{2026}",
    tab_rename_title: "重命名标签",
    tab_rename_placeholder: "标签名称",
    tab_rename_hint: "名称只替换自动标题；目标、类型和状态仍显示在悬停提示与标签列表里。",
    tab_rename_save: "保存名称",
    tab_rename_empty: "这里需要一个名称；要清除请用「恢复自动标题」。",
    tab_rename_auto: "恢复自动标题",
    tab_rename_limit: "标签名称最多 {max} 个字符；保存时只保留前 {max} 个，其余不保存。",
    tab_rename_shortened: "已保存前 {max} 个字符；修改名称后再次保存可更新。",
    tab_accent_label: "标签辨识色",
    tab_accent_none: "不设颜色",
    tab_accent_iris: "鸢尾蓝",
    tab_accent_violet: "紫罗兰",
    tab_accent_teal: "青绿",
    tab_accent_lime: "黄绿",
    tab_accent_orchid: "兰紫",
    tab_accent_clay: "陶土",
    tab_list_title: "全部标签",
    tabs_scroll_start: "向左滚动标签",
    tabs_scroll_end: "向右滚动标签",
    tab_list_hint: "按顶栏顺序列出全部标签，选择即切换。",
    tab_mark_bell: "响铃",
    tab_mark_finished: "结束",
    tab_mark_bell_hint: "后台 Shell 响铃了。打开这个标签即清除标记；响铃字节照常送到了它的程序。",
    tab_mark_finished_hint: "Shell 报告后台的一条命令已结束。打开这个标签即清除标记。",
    pane_missing_host: "这台主机没有保存",
    pane_missing_next: "修复主机或移除这个窗格。没有建立连接。",
    workspace_dangling_tab: "保存的活动标签不在这个工作区里。请另选一个标签，其余标签已保留。",
    workspace_dangling_pane: "保存的活动窗格不在它的标签上。请另选一个窗格，布局已保留。",
    pane_mode_classic: "经典终端",
    pane_mode_integrated: "命令块模式，内容未恢复",
    terminal_mode_blocks: "命令块",
    terminal_block_running: "进行中",
    terminal_block_unknown: "未知",
    terminal_block_exit: "退出码",
    terminal_block_copy: "复制此块",
    terminal_block_rerun: "重新运行",
    terminal_search: "搜索这个终端",
    terminal_search_empty: "这个终端里没有匹配",
    terminal_search_invalid: "正则表达式无效",
    terminal_search_next: "下一个匹配",
    terminal_search_previous: "上一个匹配",
    terminal_search_case: "区分大小写",
    terminal_search_regex: "正则表达式",
    terminal_search_close: "关闭搜索",
    terminal_search_keys: "Enter 下一个 · Esc 关闭",
    terminal_search_keyboard_terminal: "键盘已回到终端",
    terminal_paste_title: "确认粘贴",
    terminal_paste_target: "目标会话",
    terminal_paste_confirm: "粘贴到这个会话",
    terminal_paste_cancel: "不粘贴",
    terminal_paste_lines: "行",
    terminal_paste_runs: "确认后执行",
    terminal_paste_run_confirm: "执行这条命令",
    terminal_paste_safe: "按 Esc 或「不粘贴」不会写入任何内容，键盘回到终端。",
    terminal_no_blocks: "这个 Shell 没有报告命令块。网格保持原样。",
    terminal_blocks_failed: "Shell 集成已失效。经典网格已保留。",
    terminal_filter_all: "全部命令",
    terminal_filter_running: "仅进行中",
    terminal_filter_unknown: "未知退出",
    terminal_filter_failed: "非零退出",
    terminal_filter_zero: "仅退出码 0",
    nav_forwards: "转发",
    nav_forwards_summary: "本地、远程与动态隧道",
    empty_forwards_title: "还没有转发规则",
    empty_forwards_body: "规则保存在这里，直到你启动。打开这一页不会监听。",
    forward_saved: "已保存",
    forward_starting: "正在启动",
    forward_listening: "监听中",
    forward_stopped: "已停止",
    forward_failed: "未在监听",
    forward_start: "启动",
    forward_stop: "停止",
    forward_save: "保存规则",
    forward_delete: "删除",
    forward_edit: "编辑",
    forward_exposure_title: "监听地址不是回环",
    forward_exposure_all: "绑定全部网卡后，能访问这台电脑的其他机器都可以连入。确认之前不会启动。",
    forward_exposure_named: "这个地址不是回环。能访问它的机器都可以连入。确认之前不会启动。",
    forward_exposure_confirm: "在这个地址上监听",
    forward_exposure_cancel: "只保留规则",
    forward_close_rules: "关闭这个连接只会停止这些转发",
    forward_close_app: "关闭应用会停止这些转发",
    forward_open: "转发",
    forward_carrier_lost: "连接已关闭，这条转发不再监听。",
    forward_connect_only: "只建立转发连接，不打开终端",
};

/// Localized label for a session lifecycle state.
///
/// English reuses the domain label so the two cannot drift; Chinese is written
/// here because the domain crate stays language-neutral.
pub fn session_state_label(state: &SessionState, locale: Locale) -> String {
    // A process that ended the way it was asked to is not a disconnect: the tab, the
    // session bar, the list row and the notice all say the same word for it, so one
    // session never reports two different things about the same end (R17).
    if let SessionState::Disconnected(failure) = state
        && failure.ended_as_expected()
    {
        return locale.strings().terminal_exited.to_owned();
    }
    match locale {
        Locale::English => state.label().to_owned(),
        Locale::Chinese => match state {
            SessionState::Idle => "未开始",
            SessionState::Connecting => "连接中",
            SessionState::VerifyingHost => "校验主机身份",
            SessionState::Authenticating => "认证中",
            SessionState::Connected => "已连接",
            SessionState::Disconnected(_) => "已断开",
            SessionState::Failed(_) => "失败",
            SessionState::Closed => "已关闭",
        }
        .to_owned(),
    }
}

/// Localized label for a transfer state.
pub fn transfer_state_label(state: &TransferState, locale: Locale) -> String {
    match state {
        TransferState::Queued => match locale {
            Locale::English => "Queued".to_owned(),
            Locale::Chinese => "排队中".to_owned(),
        },
        TransferState::AwaitingOverwrite { .. } => match locale {
            Locale::English => "Waiting for your choice".to_owned(),
            Locale::Chinese => "等待你的选择".to_owned(),
        },
        TransferState::Running { .. } => match locale {
            Locale::English => "Running".to_owned(),
            Locale::Chinese => "进行中".to_owned(),
        },
        TransferState::Completed { .. } => match locale {
            Locale::English => "Completed".to_owned(),
            Locale::Chinese => "已完成".to_owned(),
        },
        TransferState::Failed { .. } => match locale {
            Locale::English => "Failed".to_owned(),
            Locale::Chinese => "失败".to_owned(),
        },
        TransferState::Cancelled { .. } => match locale {
            Locale::English => "Cancelled".to_owned(),
            Locale::Chinese => "已取消".to_owned(),
        },
    }
}

/// The next step an SSH failure suggests, in the interface language.
///
/// The SSH layer words its advice in English and does not know the interface language;
/// its sentence is the key here. A sentence this table does not know is shown as it
/// came, so a new hint is never hidden, only untranslated until it is added (a test
/// checks that every hint the SSH layer has today is here).
#[must_use]
pub fn next_step_text(english: &str, locale: Locale) -> &str {
    match locale {
        Locale::English => english,
        Locale::Chinese => match english {
            "Check the address and port, then connect again." => "检查地址和端口，然后重新连接。",
            "Check the network and the address, then try again." => "检查网络和地址，然后重试。",
            "Check the address, then try again." => "检查地址，然后重试。",
            "The connection stops here; nothing was stored." => "连接到此为止，没有保存任何内容。",
            "Connect again and accept or refuse the identity while it is shown." => {
                "重新连接，并在身份信息显示时选择接受或拒绝。"
            }
            "Do not continue. Check the server identity before trusting it again." => {
                "不要继续。重新信任之前，先核实服务器身份。"
            }
            "The server key cannot be compared; connect with a client that supports it." => {
                "无法比对这台服务器的密钥；请改用支持它的客户端连接。"
            }
            "Check the credentials, or change the authentication method." => {
                "检查凭据，或者换一种登录方式。"
            }
            "Choose a key file this account can read." => "选择一个本账号可以读取的私钥文件。",
            "Enter the key passphrase to continue." => "输入私钥口令后继续。",
            "Enter the correct passphrase for this key." => "输入这个私钥的正确口令。",
            "Choose a key file Shellune can read." => "选择一个 Shellune 能识别的私钥文件。",
            "Start the system SSH agent, then connect again." => {
                "启动系统 SSH Agent，然后重新连接。"
            }
            "Load a key into the agent, then connect again." => {
                "把私钥加载到 Agent 中，然后重新连接。"
            }
            "Use another identity, or a key file instead." => "换一个身份，或改用私钥文件。",
            "Load an identity the server accepts." => "加载一个服务器接受的身份。",
            "The server refused a terminal; use another account." => {
                "服务器拒绝分配终端；请换一个账号。"
            }
            "Connect again to open a new session." => "重新连接以打开新的会话。",
            "Fix the jump chain or the ProxyCommand, then connect again." => {
                "修正跳板链或 ProxyCommand，然后重新连接。"
            }
            "Connect again to start a new attempt." => "重新连接以开始一次新的尝试。",
            "Confirm the command to run it, or remove the ProxyCommand from the host." => {
                "确认后才会运行这条命令，或者从主机上去掉 ProxyCommand。"
            }
            other => other,
        },
    }
}

// ---------------------------------------------------------------------------
// Copy of the redesigned shell: hosts home, settings, connection dialogs and the
// host tools (port forwarding, files, transfers).
// ---------------------------------------------------------------------------

/// Strings of the redesigned shell, kept apart from [`Strings`] so the two can be
/// read side by side.
pub struct UxStrings {
    /// Settings
    pub settings_title: &'static str,
    /// Appearance, language and terminal
    pub settings_summary: &'static str,
    /// New tab
    pub new_tab: &'static str,
    /// More ways to open a tab
    pub new_tab_menu: &'static str,
    /// Placeholder of the new-tab launcher's search
    pub launcher_placeholder: &'static str,
    /// Launcher row that connects to a typed address without saving it
    pub launcher_connect: &'static str,
    /// Search hosts and commands
    pub search: &'static str,
    /// This computer
    pub home_local_section: &'static str,
    /// Opens in a new tab
    pub home_local_hint: &'static str,
    /// Servers
    pub home_hosts_section: &'static str,
    /// Add your first server
    pub home_welcome_title: &'static str,
    /// Enter the address, user and sign-in once, then connect with one click. Just trying a server? Quick connect does not save anything.
    pub home_welcome_body: &'static str,
    /// Import from SSH config
    pub home_import: &'static str,
    /// No terminal is open
    pub terminals_empty_title: &'static str,
    /// Open a shell on this computer or connect to a server from Hosts.
    pub terminals_empty_body: &'static str,
    /// Appearance
    pub settings_appearance: &'static str,
    /// Theme
    pub settings_theme: &'static str,
    /// Follow the system, or stay light or dark
    pub settings_theme_hint: &'static str,
    /// Language
    pub settings_language: &'static str,
    /// Applies at once, including inside terminals
    pub settings_language_hint: &'static str,
    /// Interface text size
    pub settings_text_size: &'static str,
    /// Scales every label, button and list
    pub settings_text_size_hint: &'static str,
    /// Motion
    pub settings_motion: &'static str,
    /// Reduced skips short fades; so does the system's reduce-motion setting
    pub settings_motion_hint: &'static str,
    /// Notifications about the tabs that are not on screen
    pub settings_notices: &'static str,
    /// Bell reminder
    pub settings_bell_notice: &'static str,
    /// Label of the switch that colours addresses, links and failures in output
    pub settings_highlight: &'static str,
    /// Label of the switch that shows each host's operating system on its tile
    pub settings_os_icons: &'static str,
    /// What that switch does
    pub settings_os_icons_hint: &'static str,
    /// What that switch does
    pub settings_highlight_hint: &'static str,
    /// Marks the tab; the bell byte still reaches the program
    pub settings_bell_notice_hint: &'static str,
    /// Command-end reminder
    pub settings_finished_notice: &'static str,
    /// Marks the tab only when the shell reports the end
    pub settings_finished_notice_hint: &'static str,
    /// A reminder that is on
    pub notice_on: &'static str,
    /// A reminder that is off
    pub notice_off: &'static str,
    /// Terminal
    pub settings_terminal: &'static str,
    /// Font size
    pub settings_font_size: &'static str,
    /// Used by new terminals and the open ones
    pub settings_font_size_hint: &'static str,
    /// Default shell
    pub settings_default_shell: &'static str,
    /// What the + button opens
    pub settings_default_shell_hint: &'static str,
    /// Data
    pub settings_data: &'static str,
    /// Saved hosts
    pub settings_hosts_file: &'static str,
    /// Adds hosts from ~/.ssh/config after a preview
    pub settings_import_hint: &'static str,
    /// Version
    pub settings_version: &'static str,
    /// Connection
    pub settings_connection: &'static str,
    /// Keepalive and timeout
    pub settings_connection_policy: &'static str,
    /// The policy below is what every connection is dialled with
    pub settings_connection_hint: &'static str,
    /// Keepalive interval
    pub settings_keepalive_interval: &'static str,
    /// How long a silent connection waits before a probe
    pub settings_keepalive_interval_hint: &'static str,
    /// Unanswered probes
    pub settings_keepalive_misses: &'static str,
    /// How many probes may go unanswered before the connection is dropped
    pub settings_keepalive_misses_hint: &'static str,
    /// Connect timeout
    pub settings_connect_timeout: &'static str,
    /// How long reaching the host may take
    pub settings_connect_timeout_hint: &'static str,
    /// The policy in one line, with the three numbers filled in
    pub connection_summary: &'static str,
    /// The number of seconds without a reply before the connection counts as lost, with
    /// that number filled in
    pub connection_window: &'static str,
    /// Keepalive is off, so a silent link is not noticed
    pub connection_window_off: &'static str,
    /// What the interval and the probe count add up to
    pub connection_window_hint: &'static str,
    /// The global values are what a host without its own numbers is dialled with
    pub connection_defaults_note: &'static str,
    /// Keepalive
    pub host_connection_title: &'static str,
    /// Inherit the global default
    pub host_connection_inherit: &'static str,
    /// Off
    pub host_connection_off: &'static str,
    /// Override
    pub host_connection_override: &'static str,
    /// What inheriting means, with the values filled in
    pub host_connection_inherits: &'static str,
    /// Inheriting a global policy whose keepalive is off; nothing is probed.
    pub host_connection_inherits_off: &'static str,
    /// What switching keepalive off for one host means
    pub host_connection_off_note: &'static str,
    /// What an override applies to
    pub host_connection_override_note: &'static str,
    /// What an import applied, with the numbers filled in
    pub import_connection: &'static str,
    /// An import that turned keepalive off
    pub import_connection_off: &'static str,
    /// An import that set only part of the policy
    pub import_connection_partial: &'static str,
    /// A field was left empty
    pub connection_problem_missing: &'static str,
    /// The text is not a whole number
    pub connection_problem_number: &'static str,
    /// The text is below zero
    pub connection_problem_negative: &'static str,
    /// The interval is above what this build stores
    pub connection_problem_interval: &'static str,
    /// The probe count is below one or above what this build stores
    pub connection_problem_misses: &'static str,
    /// The connect timeout is below one or above what this build stores
    pub connection_problem_timeout: &'static str,
    /// Connect to
    pub connect_title: &'static str,
    /// Choose how this server checks it is you. A password is only kept if you tick Remember.
    pub connect_hint: &'static str,
    /// Connect without saving. You can save the host afterwards from the session head.
    pub quick_hint: &'static str,
    /// Close
    pub tool_close: &'static str,
    /// Keep connecting in the background
    pub connect_background: &'static str,
    /// Port forwarding
    pub tool_forwards_title: &'static str,
    /// Every saved tunnel
    pub tool_forwards_all: &'static str,
    /// Tunnels through
    pub tool_forwards_host: &'static str,
    /// Transfers
    pub tool_transfers_title: &'static str,
    /// Uploads and downloads of every connection
    pub tool_transfers_subtitle: &'static str,
    /// New tunnel
    pub fwd_new: &'static str,
    /// Edit tunnel
    pub fwd_edit: &'static str,
    /// Type
    pub fwd_type: &'static str,
    /// Local
    pub fwd_kind_local: &'static str,
    /// Remote
    pub fwd_kind_remote: &'static str,
    /// SOCKS proxy
    pub fwd_kind_dynamic: &'static str,
    /// Reach a service the server can see (a database, an admin page) from this computer.
    pub fwd_kind_local_hint: &'static str,
    /// Let the server reach a service running on this computer.
    pub fwd_kind_remote_hint: &'static str,
    /// Send apps through the server: set their SOCKS proxy to the local port.
    pub fwd_kind_dynamic_hint: &'static str,
    /// Through host
    pub fwd_through: &'static str,
    /// Placeholder of the search in the through-host picker.
    pub fwd_host_search: &'static str,
    /// The through-host picker's line when no host matches its search.
    pub fwd_host_no_match: &'static str,
    /// The through-host control before a host is chosen.
    pub fwd_host_choose: &'static str,
    /// On this computer
    pub fwd_listen_local: &'static str,
    /// On the server
    pub fwd_listen_remote: &'static str,
    /// Destination, as the server sees it
    pub fwd_target_remote: &'static str,
    /// Destination on this computer
    pub fwd_target_local: &'static str,
    /// Opening {listen} on this computer reaches {target} through {host}.
    pub fwd_preview_local: &'static str,
    /// Opening {listen} on {host} reaches {target} on this computer.
    pub fwd_preview_remote: &'static str,
    /// Apps whose SOCKS proxy is {listen} go out through {host}.
    pub fwd_preview_dynamic: &'static str,
    /// No port forwarding yet
    pub fwd_empty_title: &'static str,
    /// A tunnel carries one port through an SSH host, so a service on one side can be used from the other.
    pub fwd_empty_body: &'static str,
    /// Save a host first: every tunnel runs through an SSH host.
    pub fwd_no_hosts: &'static str,
    /// Cancel
    pub fwd_cancel: &'static str,
    /// Question before a running tunnel is stopped
    pub fwd_stop_title: &'static str,
    /// The confirm button's own action, not the question it answers.
    pub fwd_stop_action: &'static str,
    /// What stopping does, with the endpoint filled in
    pub fwd_stop_body: &'static str,
    /// Question before a tunnel is deleted
    pub fwd_delete_title: &'static str,
    /// The confirm button's own action, not the question it answers.
    pub fwd_delete_action: &'static str,
    /// What deleting does, with the tunnel and its state filled in
    pub fwd_delete_body: &'static str,
    /// Said in the delete question of a tunnel that is listening now
    pub fwd_delete_running: &'static str,
    /// Said in the delete question of a tunnel that is not listening
    pub fwd_delete_saved: &'static str,
    /// Said of the side that holds the listening port of a local forward
    pub fwd_here: &'static str,
    /// Said of the side the connection reaches for a local forward
    pub fwd_there: &'static str,
    /// Failure: the local port belongs to another program
    pub fwd_failure_in_use: &'static str,
    /// Failure: this account may not listen on that port
    pub fwd_failure_denied: &'static str,
    /// Failure: the server refused the tunnel; the reason is filled in
    pub fwd_failure_server: &'static str,
    /// Failure: the target did not accept a connection; the target is filled in
    pub fwd_failure_target: &'static str,
    /// Duplicate
    pub fwd_more: &'static str,
    /// Name
    pub files_name: &'static str,
    /// Size
    pub files_size: &'static str,
    /// Modified
    pub files_modified: &'static str,
    /// This computer
    pub files_this_computer: &'static str,
    /// Ports
    pub session_forwards: &'static str,
    pub custom_shell_title: &'static str,
    pub drop_upload_failed: &'static str,
    pub drop_folders_unsupported: &'static str,
    pub drop_uploading: &'static str,
    pub toast_uploaded: &'static str,
    pub toast_downloaded: &'static str,
    pub toast_path_copied: &'static str,
    pub files_open: &'static str,
    pub files_copy_path: &'static str,
    /// Tooltip of the box that marks one row of a file column
    pub files_mark_row: &'static str,
    /// Tooltip of the same box once the row is marked
    pub files_unmark_row: &'static str,
    pub drop_uploading_files: &'static str,
    pub settings_scheme: &'static str,
    pub settings_scheme_hint: &'static str,
    pub settings_scheme_app: &'static str,
    pub custom_shell_body: &'static str,
    pub custom_shell_run: &'static str,
    /// Keyboard shortcuts group of the settings page
    pub settings_keys: &'static str,
    pub settings_keys_hint: &'static str,
    /// Shown while the category column has the keyboard.
    pub settings_nav_hint: &'static str,
    pub keys_change: &'static str,
    pub keys_unbind: &'static str,
    pub keys_reset: &'static str,
    pub keys_reset_all: &'static str,
    pub keys_unbound: &'static str,
    pub keys_changed: &'static str,
    pub keys_default: &'static str,
    pub keys_capture_title: &'static str,
    pub keys_capture_hint: &'static str,
    pub keys_capture_cancel: &'static str,
    pub keys_no_match: &'static str,
    pub keys_custom_summary: &'static str,
    pub keys_problem_hint: &'static str,
    pub keys_save_failed: &'static str,
    pub keys_save_failed_next: &'static str,
    /// Label above the scope filter of the shortcut page
    pub keys_scope_label: &'static str,
    /// The scope filter's "every scope" choice
    pub keys_scope_all: &'static str,
    /// What to do about two actions that answer one combination
    pub keys_conflict_next: &'static str,
    /// Title of the shortcut-command manager
    pub snippet_manage_title: &'static str,
    /// What filling in and running do
    pub snippet_manage_hint: &'static str,
    /// Where the commands are stored, and what must not be kept there
    pub snippet_storage_note: &'static str,
    /// Button: write a new command
    pub snippet_new: &'static str,
    /// Title of the editor when changing a command
    pub snippet_edit_title: &'static str,
    /// Label of the name field
    pub snippet_field_name: &'static str,
    /// Label of the command field
    pub snippet_field_body: &'static str,
    /// Placeholder of the name field
    pub snippet_name_placeholder: &'static str,
    /// Placeholder of the command field; names the key that adds a line
    pub snippet_body_placeholder: &'static str,
    /// Editor key hint under the fields
    pub snippet_editor_keys: &'static str,
    /// Button: save the command
    pub snippet_save: &'static str,
    /// Row button: write the command into the terminal without running it
    pub snippet_insert: &'static str,
    /// Row button: write the command and submit it once
    pub snippet_run: &'static str,
    /// Said beside the fill-in action: the text is written and nothing runs
    pub snippet_fill_hint: &'static str,
    /// Said beside the run action: it is written and submitted once
    pub snippet_run_hint: &'static str,
    /// Row button: change the command
    pub snippet_edit: &'static str,
    /// Row button: remove the command
    pub snippet_delete: &'static str,
    /// Question before a command is removed
    pub snippet_delete_title: &'static str,
    /// What deleting does, with the name filled in
    pub snippet_delete_body: &'static str,
    /// Answer that deletes the command
    pub snippet_delete_yes: &'static str,
    /// Answer that keeps the command
    pub snippet_delete_no: &'static str,
    /// Empty state of the manager
    pub snippet_empty_title: &'static str,
    /// What to do first when nothing is saved
    pub snippet_empty_body: &'static str,
    /// Toast and note: no terminal is open
    pub snippet_no_terminal: &'static str,
    /// Next step after that note
    pub snippet_no_terminal_body: &'static str,
    /// Toast and note: the terminal on screen has no running shell
    pub snippet_terminal_ended: &'static str,
    /// Next step after that note
    pub snippet_terminal_ended_body: &'static str,
    /// Toast: the command was typed into the terminal
    pub snippet_inserted: &'static str,
    /// Toast: the command was sent to the terminal
    pub snippet_sent: &'static str,
    /// Toast: the command is held in the pane's paste prompt, nothing written yet
    pub snippet_waiting: &'static str,
    /// What the prompt asks for next, after the note above
    pub snippet_waiting_body: &'static str,
    /// Toast: the command was saved
    pub snippet_saved: &'static str,
    /// Toast: the command was deleted
    pub snippet_deleted: &'static str,
    /// Title shown when the file could not be read
    pub snippet_unreadable_title: &'static str,
    /// What happens to the unreadable file, with the path filled in
    pub snippet_unreadable_body: &'static str,
    /// Why a save did not happen, with the storage reason filled in
    pub snippet_save_failed_detail: &'static str,
    /// Line count of a command body, with the number filled in
    pub snippet_line_count: &'static str,
    /// How many commands are saved, with the number filled in
    pub snippet_count: &'static str,
    /// Palette action: open the manager
    pub snippet_manage_action: &'static str,
    /// Palette action: start a new command
    pub snippet_new_action: &'static str,
    /// Palette entry for inserting a command, with its name filled in
    pub snippet_insert_action: &'static str,
    /// Palette entry for running a command, with its name filled in
    pub snippet_run_action: &'static str,
    /// Refusal: the name is empty
    pub snippet_problem_name_empty: &'static str,
    /// Refusal: the name is too long, with the limit filled in
    pub snippet_problem_name_long: &'static str,
    /// Refusal: the body is empty
    pub snippet_problem_body_empty: &'static str,
    /// Refusal: the body is too long, with the limit filled in
    pub snippet_problem_body_long: &'static str,
    /// Refusal: the command is no longer in the library
    pub snippet_problem_unknown: &'static str,
    /// Heading of the group column on a wide window
    pub hosts_groups_title: &'static str,
    /// Hover on a host row's connect button: the faster way in
    pub hosts_connect_hint: &'static str,
    /// Example a host form's empty group field shows
    pub host_example_group: &'static str,
    /// Hover on a value that one click copies
    pub copy_click_hint: &'static str,
    /// Toast after a value was copied
    pub toast_copied: &'static str,
    /// Connection progress: reaching the server
    pub connect_step_dial: &'static str,
    /// Connection progress: checking the server's identity
    pub connect_step_identity: &'static str,
    /// Connection progress: signing in
    pub connect_step_login: &'static str,
    /// Connection progress: opening the terminal
    pub connect_step_terminal: &'static str,
    /// Connection form: reveals the less common options
    pub connect_more_options: &'static str,
    /// Connection failure that was not about signing in: open the sign-in form anyway
    pub connect_change_sign_in: &'static str,
    /// Sign-in method: try what the computer has
    pub auth_auto: &'static str,
    /// What the automatic sign-in tries
    pub auth_auto_hint: &'static str,
    /// Host dialog: under its title
    pub host_dialog_intro: &'static str,
    /// Host dialog: name field, which may stay empty
    pub host_name_optional: &'static str,
    /// Host dialog: group field, which may stay empty
    pub host_group_optional: &'static str,
    /// Host dialog: the password of the automatic method
    pub host_password_optional: &'static str,
    /// Label of the user name a VNC server asks for only sometimes.
    pub host_user_optional: &'static str,
    /// Host dialog: the key file is protected by a passphrase
    pub host_key_has_passphrase: &'static str,
    /// Host dialog: the folded section
    pub host_advanced_title: &'static str,
    /// Host dialog: what the folded section holds
    pub host_advanced_hint: &'static str,
    /// Host dialog: something in the folded section is set
    pub host_advanced_in_use: &'static str,
    /// Host dialog: the group of where and who
    pub host_group_basic: &'static str,
    /// Host dialog: the group of jump hosts and the ProxyCommand
    pub host_group_route: &'static str,
    /// Host dialog: the group of the transfer protocol
    pub host_group_transfer: &'static str,
    /// Connection dialog: the group of what the protocol lets the user choose
    pub host_group_options: &'static str,
    /// Host dialog: the placeholder of the jump host picker's search
    pub host_jump_search: &'static str,
    /// Host dialog: the jump host picker has no saved SSH host left to offer
    pub host_jump_none_left: &'static str,
    /// Host dialog: the jump host picker's search matches no host
    pub host_jump_no_match: &'static str,
    /// Group action: create a group
    pub hosts_group_new: &'static str,
    /// Group action: rename a group
    pub hosts_group_rename: &'static str,
    /// Group action: move the selected hosts into a group
    pub hosts_group_move: &'static str,
    /// Group action: delete a group, moving its hosts somewhere else
    pub hosts_group_delete: &'static str,
    /// Placeholder of the field that narrows an import preview
    pub import_search_placeholder: &'static str,
    /// How many hosts a preview shows of how many it describes
    pub import_count: &'static str,
    /// What an import preview with more rows than it draws says about the rest
    pub import_row_limit: &'static str,
    /// Field label: the group name
    pub hosts_group_name_label: &'static str,
    /// Refusal: the dialog was confirmed with no name
    pub hosts_group_name_required: &'static str,
    /// Which destination means "no group at all"
    pub hosts_group_destination_hint: &'static str,
    /// Button: create the named group
    pub hosts_group_create_action: &'static str,
    /// Button: rename the group
    pub hosts_group_rename_action: &'static str,
    /// Button: move the selected hosts into the group named `{name}`
    pub hosts_group_move_action: &'static str,
    /// Button: take the selected hosts out of every group
    pub hosts_move_ungrouped_action: &'static str,
    /// Destination row that means "no group at all"
    pub hosts_move_none_row: &'static str,
    /// Title of the move dialog for `{count}` hosts
    pub hosts_move_title: &'static str,
    /// Button: delete the group and keep its hosts
    pub hosts_group_delete_action: &'static str,
    /// What deleting a group does to its hosts, with the number filled in
    pub hosts_group_delete_body: &'static str,
    /// Toast: the group was created
    pub hosts_group_created: &'static str,
    /// Toast: the group was renamed, with the number of hosts filled in
    pub hosts_group_renamed: &'static str,
    /// Toast: the group was deleted, with the number of hosts filled in
    pub hosts_group_deleted: &'static str,
    /// Toast: hosts were moved, with the number filled in
    pub hosts_moved: &'static str,
    /// Label of the status filter
    pub hosts_status_label: &'static str,
    /// Status filter: every host
    pub hosts_status_any: &'static str,
    /// Status filter: a host with an open session
    pub hosts_status_connected: &'static str,
    /// Status filter: a host that was reached before
    pub hosts_status_recent: &'static str,
    /// Status filter: a host the book never recorded a connection for
    pub hosts_status_never: &'static str,
    /// Label of the protocol filter
    pub hosts_protocol_label: &'static str,
    /// Protocol filter: every protocol
    pub hosts_protocol_all: &'static str,
    /// Sidebar group locator placeholder
    pub hosts_group_search_placeholder: &'static str,
    /// Sidebar group locator empty result
    pub hosts_group_no_match: &'static str,
    /// Sidebar group locator clear button
    pub hosts_group_search_clear: &'static str,
    /// Multi-select scope boundary note: batch & move apply to SSH hosts only
    pub hosts_selection_ssh_only: &'static str,
    /// Group selector row that keeps a typed path that no saved group has
    pub hosts_group_picker_create: &'static str,
    /// Validation: port is not a number
    pub validation_port_not_number: &'static str,
    /// Validation: port out of range
    pub validation_port_range: &'static str,
    /// Validation: address port conflicts with port field
    pub validation_address_port_conflict: &'static str,
    /// Validation: baud is not a number
    pub validation_baud_not_number: &'static str,
    /// Validation: baud out of range
    pub validation_baud_range: &'static str,
    /// Validation: data bits is not a number
    pub validation_data_bits_not_number: &'static str,
    /// Validation: data bits out of range
    pub validation_data_bits_range: &'static str,
    /// Validation: address is empty
    pub validation_address_empty: &'static str,
    /// Validation: address contains whitespace
    pub validation_address_whitespace: &'static str,
    /// Validation: user is empty
    pub validation_user_empty: &'static str,
    /// Validation: user contains whitespace
    pub validation_user_whitespace: &'static str,
    /// Validation: name is empty
    pub validation_name_empty: &'static str,
    /// Validation: serial device is empty
    pub validation_device_empty: &'static str,
    /// Validation: field contains control characters
    pub validation_control_char: &'static str,
    /// Validation: group path is invalid
    pub validation_group_invalid: &'static str,
    /// Scope: the hosts the current search and filters keep
    pub hosts_scope_current: &'static str,
    /// Scope: every saved host
    pub hosts_scope_all: &'static str,
    /// How many hosts are selected, with the number filled in
    pub hosts_selection_count: &'static str,
    /// Button: select every host the scope names
    pub hosts_select_scope: &'static str,
    /// Button: empty the selection
    pub hosts_select_none: &'static str,
    /// Button: turn on the selection boxes
    pub hosts_select_start: &'static str,
    /// Button: clear the search and every filter
    pub hosts_filter_clear: &'static str,
    /// What a narrowing filter leaves behind when nothing matches
    pub hosts_no_match_hint: &'static str,
    /// The group column when no group has been made yet
    pub hosts_group_empty: &'static str,
    /// Button: open the batch task preview for the selected hosts
    pub hosts_batch_open: &'static str,
    /// Hint: what a batch task does
    pub hosts_batch_open_hint: &'static str,
    /// Title of the batch panel
    pub batch_title: &'static str,
    /// Subtitle of the preview: nothing connects before confirmation
    pub batch_preview_subtitle: &'static str,
    /// Section label: the hosts the task would run on
    pub batch_targets_section: &'static str,
    /// How many hosts would run, with the number filled in
    pub batch_targets_count: &'static str,
    /// Scope label inside the preview
    pub batch_scope_label: &'static str,
    /// Note above the selected hosts that could not be included
    pub batch_targets_refused: &'static str,
    /// How many target rows the preview does not list, with the number filled in
    pub batch_targets_more: &'static str,
    /// Section label: the commands, in order
    pub batch_steps_section: &'static str,
    /// Button: add one more command step
    pub batch_step_add: &'static str,
    /// Button: remove one command step
    pub batch_step_remove: &'static str,
    /// Placeholder of a command field
    pub batch_step_placeholder: &'static str,
    /// Label of the field that limits how many hosts run at once
    pub batch_concurrency: &'static str,
    /// Hint under that field, with the limit filled in
    pub batch_concurrency_hint: &'static str,
    /// Label of the failure policy control
    pub batch_failure: &'static str,
    /// Failure policy: skip that host, keep the others
    pub batch_failure_skip: &'static str,
    /// Failure policy: stop scheduling new hosts
    pub batch_failure_stop: &'static str,
    /// Label of the optional task name
    pub batch_name: &'static str,
    /// Placeholder of the task name
    pub batch_name_placeholder: &'static str,
    /// Button: confirm the task and start the run
    pub batch_confirm: &'static str,
    /// Button: put the preview away
    pub batch_cancel: &'static str,
    /// Button: close the run page
    pub batch_close: &'static str,
    /// Run state: running
    pub batch_run_running: &'static str,
    /// Run state: paused, and what a pause leaves alone
    pub batch_run_paused: &'static str,
    /// Run state: cancelling, and that the remote command has not ended yet
    pub batch_run_cancelling: &'static str,
    /// Run state: cancelled
    pub batch_run_cancelled: &'static str,
    /// Run state: completed
    pub batch_run_completed: &'static str,
    /// Run state: finished with failures
    pub batch_run_failed: &'static str,
    /// Run state: interrupted by a restart
    pub batch_run_interrupted: &'static str,
    /// Progress line, with finished, total, running and concurrency filled in
    pub batch_run_progress: &'static str,
    /// Button: pause the run
    pub batch_run_pause: &'static str,
    /// Button: continue a paused run
    pub batch_run_resume: &'static str,
    /// Button: ask the run to stop
    pub batch_run_cancel: &'static str,
    /// Button: retry the hosts that did not succeed
    pub batch_run_retry: &'static str,
    /// Section label above a host's output
    pub batch_run_output: &'static str,
    /// What the output pane says before a host printed anything
    pub batch_run_output_empty: &'static str,
    /// Note above a bounded output log, with the number of dropped lines filled in
    pub batch_run_output_dropped: &'static str,
    /// Section label above the per-step times
    pub batch_run_timeline: &'static str,
    /// One step of the timeline, with the one-based number filled in
    pub batch_run_step: &'static str,
    /// What a retry does not do, said where the retry is offered
    pub batch_no_rollback: &'static str,
    /// Note on a preview that is a retry
    pub batch_retry_note: &'static str,
    /// The question asked about an unknown host identity
    pub batch_trust_question: &'static str,
    /// Button: accept the presented identity
    pub batch_trust_accept: &'static str,
    /// Button: decline the presented identity
    pub batch_trust_decline: &'static str,
    /// The question asked about a local ProxyCommand
    pub batch_proxy_question: &'static str,
    /// How many steps the task has, with the number filled in
    pub batch_count_steps: &'static str,
    /// One step result, with the exit status filled in
    pub batch_exit: &'static str,
    /// A host that starts at a later step, with the number filled in
    pub batch_from_step: &'static str,
    /// Prompt shown when a preview has no target at all
    pub batch_no_targets: &'static str,
    /// Why a host has no route: no saved sign-in
    pub batch_reason_no_credential: &'static str,
    /// Why a host has no route: the saved host is gone
    pub batch_reason_missing_host: &'static str,
    /// The task definition could not be saved, with the reason filled in
    pub batch_save_failed: &'static str,
    /// A line of the run log could not be written
    pub batch_log_failed: &'static str,
    /// Step state: waiting
    pub batch_step_pending: &'static str,
    /// Step state: running
    pub batch_step_running: &'static str,
    /// Step state: succeeded
    pub batch_step_succeeded: &'static str,
    /// Step state: failed
    pub batch_step_failed: &'static str,
    /// Step state: skipped
    pub batch_step_skipped: &'static str,
    /// Step state: cancelled
    pub batch_step_cancelled: &'static str,
    /// Step state: already done by the run this one retries
    pub batch_step_already_done: &'static str,
    /// Step state: interrupted by a restart
    pub batch_step_interrupted: &'static str,
    /// Host state: queued
    pub batch_host_queued: &'static str,
    /// Host state: connecting
    pub batch_host_connecting: &'static str,
    /// Host state: running
    pub batch_host_running: &'static str,
    /// Host state: waiting for the user's answer
    pub batch_host_waiting: &'static str,
    /// Host state: succeeded
    pub batch_host_succeeded: &'static str,
    /// Host state: failed
    pub batch_host_failed: &'static str,
    /// Host state: cancelled
    pub batch_host_cancelled: &'static str,
    /// Host state: skipped
    pub batch_host_skipped: &'static str,
    /// Host state: interrupted
    pub batch_host_interrupted: &'static str,
    /// Segmented switch: the saved tasks
    pub batch_switch_library: &'static str,
    /// Segmented switch: the run this window is driving
    pub batch_switch_run: &'static str,
    /// Segmented switch: the stored runs
    pub batch_switch_history: &'static str,
    /// Segmented switch: the definition being edited
    pub batch_switch_editor: &'static str,
    /// The top bar control that opens the task centre
    pub batch_center_open: &'static str,
    /// Hint under the top bar control
    pub batch_center_open_hint: &'static str,
    /// Title of the saved tasks
    pub batch_library_title: &'static str,
    /// Subtitle of the saved tasks
    pub batch_library_subtitle: &'static str,
    /// Empty state of the saved tasks: title
    pub batch_library_empty_title: &'static str,
    /// Empty state of the saved tasks: body
    pub batch_library_empty_body: &'static str,
    /// Button: a new definition from the current selection
    pub batch_library_new: &'static str,
    /// Button: edit one saved definition
    pub batch_library_edit: &'static str,
    /// Button: delete one saved definition
    pub batch_library_delete: &'static str,
    /// Question: delete this saved task?
    pub batch_library_delete_ask: &'static str,
    /// Answer: delete it
    pub batch_library_delete_yes: &'static str,
    /// Answer: keep it
    pub batch_library_delete_no: &'static str,
    /// Toast: a saved task was deleted
    pub batch_library_deleted: &'static str,
    /// Row meta: the hosts, steps and concurrency of a saved task
    pub batch_library_row_meta: &'static str,
    /// How many saved tasks the library does not list, with the number filled in
    pub batch_library_more: &'static str,
    /// A saved task that has no name
    pub batch_untitled: &'static str,
    /// Toast: the definition was saved and nothing ran
    pub batch_saved: &'static str,
    /// Button: save the definition without running it
    pub batch_save_draft: &'static str,
    /// Tooltip of the button that moves one step earlier
    pub batch_step_up: &'static str,
    /// Tooltip of the button that moves one step later
    pub batch_step_down: &'static str,
    /// Title of the stored runs
    pub batch_history_title: &'static str,
    /// Subtitle of the stored runs
    pub batch_history_subtitle: &'static str,
    /// Empty state of the stored runs
    pub batch_history_empty: &'static str,
    /// Label of the status filter
    pub batch_history_filter: &'static str,
    /// Filter value: every status
    pub batch_history_filter_any: &'static str,
    /// How many older runs are not listed
    pub batch_history_omitted: &'static str,
    /// A stored record that could not be read
    pub batch_history_damaged: &'static str,
    /// Button: open one stored run
    pub batch_history_open: &'static str,
    /// Button: back to the list of runs
    pub batch_history_back: &'static str,
    /// Hint: choose a host to read its output
    pub batch_history_no_host: &'static str,
    /// When one stored run started
    pub batch_history_when: &'static str,
    /// How many hosts of one stored run finished, out of how many
    pub batch_history_hosts: &'static str,
    /// The output of a host whose log was not read
    pub batch_history_output_unread: &'static str,
    /// The same, with the reason the log could not be read
    pub batch_history_output_unread_reason: &'static str,
    /// Button: copy the diagnostics of one run
    pub batch_diagnostics_copy: &'static str,
    /// Toast: the diagnostics were copied
    pub batch_diagnostics_copied: &'static str,
    /// Button: leave the run page and keep the run going
    pub batch_run_background: &'static str,
    /// Hint: what the background indicator does
    pub batch_run_background_hint: &'static str,
    /// The top bar indicator of a run that is still going
    pub batch_indicator_running: &'static str,
    /// The top bar indicator of a run that ended with failures
    pub batch_indicator_failed: &'static str,
    /// The run record under runs/ could not be written
    pub batch_run_save_failed: &'static str,
    /// A retry confirmation: what it will and will not do
    pub batch_retry_confirm: &'static str,
    /// Tooltip for clearing a text field
    pub field_clear: &'static str,
    /// Floating pill in a scrolled-back terminal
    pub terminal_jump_bottom: &'static str,
    /// Context-menu action to return to the live terminal prompt
    pub terminal_jump_bottom_short: &'static str,
    /// Context-menu action to select the entire terminal buffer
    pub terminal_select_all: &'static str,
    /// Tooltip on the font-size badge to reset to the default size
    pub terminal_font_reset: &'static str,
    /// Context-menu action to copy a host or connection address
    pub host_copy_target: &'static str,
    /// The placeholder of the home page's quick-connect bar
    pub home_quick_placeholder: &'static str,
    /// A quick-connect result that dials an unsaved address
    pub home_quick_temporary: &'static str,
    /// Detail of that result: nothing is saved
    pub home_quick_temporary_hint: &'static str,
    /// Keycap beside the result Enter opens
    pub home_quick_enter: &'static str,
    /// Tooltip of the keycap on the search's first match: what Enter does there
    pub home_enter_first_hint: &'static str,
    /// Label of the strip of recently reached hosts above the whole list
    pub home_recent_strip: &'static str,
    /// Tooltip of one host in that strip: connect, with when it was last reached
    pub home_recent_strip_hint: &'static str,
    /// The strip's link to the full recent view
    pub home_recent_strip_all: &'static str,
    /// The heading over the starting points a small book offers under its list.
    pub home_more_ways: &'static str,
    /// What an import from the SSH config does, under its tile.
    pub home_more_import_hint: &'static str,
    /// What a restore does, under its tile.
    pub home_more_restore_hint: &'static str,
    /// What the quick-connect tile says: typing an address into the search connects.
    pub home_more_quick: &'static str,
    /// The line under the quick-connect tile.
    pub home_more_quick_hint: &'static str,
    /// The home page's menu of local terminals
    pub home_local_button: &'static str,
    /// Heading of the serial connections in that menu
    pub home_serial_section: &'static str,
    /// Menu entry: a new serial connection
    pub home_serial_new: &'static str,
    /// The sidebar entry of the favourite records
    pub home_favorites_section: &'static str,
    /// Tooltip of the toolbar's "⋯" menu
    pub home_card_more: &'static str,
    /// Heading of the orders in the filter menu
    pub home_sort_label: &'static str,
    /// Order: as saved
    pub home_sort_saved: &'static str,
    /// Order: by name
    pub home_sort_name: &'static str,
    /// Order: most recently reached first
    pub home_sort_recent: &'static str,
    /// Tooltip of the toolbar's filter menu
    pub home_filter: &'static str,
    /// Tooltip: fold the sidebar to its icons
    pub home_sidebar_collapse: &'static str,
    /// Tooltip: unfold the sidebar
    pub home_sidebar_expand: &'static str,
    /// The favourites view when nothing is marked
    pub home_favorites_empty: &'static str,
    /// The line under it: how a record gets there
    pub home_favorites_empty_hint: &'static str,
    /// Action: mark a record as a favourite
    pub favorite_add: &'static str,
    /// Action: clear a record's favourite mark
    pub favorite_remove: &'static str,
    /// Palette entry: mark the named record as a favourite
    pub favorite_add_named: &'static str,
    /// Palette entry: clear the named record's mark
    pub favorite_remove_named: &'static str,
    /// Toast after a record was marked
    pub favorite_added: &'static str,
    /// Toast after a mark was cleared
    pub favorite_removed: &'static str,
    /// Tooltip of the terminal bar's menu that holds the tools a narrow bar folded
    pub terminal_more: &'static str,
    /// The grid size line in that menu
    pub terminal_grid_size: &'static str,
    /// The line under it: the attempt's own steps are behind the tab
    pub session_connecting_card_hint: &'static str,
    /// The banner over a terminal whose session is coming back: `{target}` (ADR-6)
    pub banner_reconnecting: &'static str,
    /// The banner's command that stops a reconnect and keeps the last screen
    pub banner_cancel: &'static str,
    /// Tooltip of the banner's close button
    pub banner_dismiss: &'static str,
    /// The title of the banner's unfolded diagnostic
    pub banner_diagnostic: &'static str,
    /// The command that copies the whole diagnostic
    pub banner_copy_diagnostic: &'static str,
    /// Palette detail suffix when a host connects through jump hosts: `{count}`
    pub palette_host_jumps: &'static str,
    /// Palette detail suffix when a host connects through a single jump host
    pub palette_host_jumps_one: &'static str,
    /// Palette detail suffix when a host uses a ProxyCommand
    pub palette_host_proxy_command: &'static str,
    /// Empty group state body when no search is active
    pub hosts_group_empty_body: &'static str,
    /// Action in selection bar to delete all selected hosts
    pub hosts_delete_selected: &'static str,
    /// Confirmation dialog title when deleting multiple selected hosts
    pub hosts_delete_selected_title: &'static str,
    /// Confirmation dialog body when deleting multiple selected hosts
    pub hosts_delete_selected_body: &'static str,
    /// Toast after deleting multiple selected hosts
    pub hosts_deleted_many: &'static str,
    /// Keyboard legend inside jump host picker
    pub host_jump_keys: &'static str,
    /// Badge shown in split-pane header while a pane fills its tab
    pub pane_zoomed: &'static str,
    /// Tooltip / label to enter cross-session focus mode
    pub focus_mode_enter: &'static str,
    /// Tooltip / label to leave cross-session focus mode
    pub focus_mode_exit: &'static str,
    /// Tooltip on split divider drag band
    pub pane_divider_reset_hint: &'static str,
    /// Tooltip hint on remote desktop zoom bar explaining Shift+scroll panning
    pub desktop_pan_hint: &'static str,
    /// Tooltip on toast dismiss button
    pub toast_dismiss: &'static str,
    /// File browser toolbar button to jump directly to a path
    pub files_go_to_path: &'static str,
    /// Inline form heading for Go to folder
    pub files_go_to_title: &'static str,
    /// Confirm button for Go to folder
    pub files_go_to_action: &'static str,
    /// Action to clear completed and cancelled transfers
    pub transfers_clear_finished: &'static str,
    /// Error shown when the tunnel's host was deleted
    pub fwd_missing_host: &'static str,
    /// Button in batch run / history output header to copy stdout/stderr
    pub batch_output_copy: &'static str,
    /// Toast after batch output was copied
    pub batch_output_copied: &'static str,
    /// Refusal tooltip when another batch run is already active
    pub batch_run_in_progress_refusal: &'static str,
    /// Batch host filter: all hosts
    pub batch_filter_all: &'static str,
    /// Batch host filter: failed hosts
    pub batch_filter_failed: &'static str,
    /// Batch host filter: waiting for user answer
    pub batch_filter_waiting: &'static str,
    /// Batch host filter: running / connecting
    pub batch_filter_running: &'static str,
    /// Batch host filter: succeeded
    pub batch_filter_done: &'static str,
    /// Placeholder for shortcut command search box
    pub snippet_search_placeholder: &'static str,
    /// Empty state when shortcut command search matches nothing
    pub snippet_search_empty: &'static str,
    /// Keyboard hint in shortcut command dialog footer
    pub snippet_keys_hint: &'static str,
    /// Placeholder for Quick Connect address field
    pub quick_address_placeholder: &'static str,
    /// Reset button for modified settings
    pub settings_reset_default: &'static str,
    /// Unsaved edits notice on WebDAV sync card
    pub sync_unsaved_changes: &'static str,
    /// Context-menu action to move a single saved record into a group
    pub host_move_to_group: &'static str,
    /// Dialog title when moving a single saved record into a group
    pub hosts_move_single_title: &'static str,
    /// Context-menu action on an SSH host to connect and open the SFTP file browser
    pub host_open_files: &'static str,
    /// Sidebar group menu action to create a new host inside that group
    pub hosts_group_new_host_here: &'static str,
    /// Sidebar group menu action to create a subgroup under that group
    pub hosts_group_new_subgroup: &'static str,
    /// Floating scroll-to-bottom pill label when new output arrived while scrolled back
    pub terminal_jump_bottom_new: &'static str,
    /// Terminal context menu action to open an http(s) URL
    pub terminal_open_link: &'static str,
    /// Terminal context menu action to copy an http(s) URL
    pub terminal_copy_link: &'static str,
    /// Terminal context menu action to clear visible screen and scrollback buffer
    pub terminal_clear_scrollback: &'static str,
    /// Menu action to open the snippets / shortcut commands dialog
    pub terminal_snippets_menu: &'static str,
    /// Searchable alias prefix for snippets in the command palette
    pub snippet_palette_aliases: &'static str,
    /// Session menu action to open global port forwarding rules
    pub session_global_forwards: &'static str,
}

impl UxStrings {
    /// Every string, for the completeness test.
    #[cfg(test)]
    #[must_use]
    pub fn all(&self) -> Vec<&'static str> {
        vec![
            self.settings_title,
            self.settings_summary,
            self.new_tab,
            self.new_tab_menu,
            self.launcher_placeholder,
            self.launcher_connect,
            self.search,
            self.home_local_section,
            self.home_local_hint,
            self.home_hosts_section,
            self.home_welcome_title,
            self.home_welcome_body,
            self.home_import,
            self.terminals_empty_title,
            self.terminals_empty_body,
            self.settings_appearance,
            self.settings_theme,
            self.settings_theme_hint,
            self.settings_language,
            self.settings_language_hint,
            self.settings_text_size,
            self.settings_text_size_hint,
            self.settings_motion,
            self.settings_motion_hint,
            self.settings_notices,
            self.settings_bell_notice,
            self.settings_highlight,
            self.settings_os_icons,
            self.settings_os_icons_hint,
            self.settings_highlight_hint,
            self.settings_bell_notice_hint,
            self.settings_finished_notice,
            self.settings_finished_notice_hint,
            self.notice_on,
            self.notice_off,
            self.settings_terminal,
            self.settings_font_size,
            self.settings_font_size_hint,
            self.settings_default_shell,
            self.settings_default_shell_hint,
            self.settings_data,
            self.settings_hosts_file,
            self.settings_import_hint,
            self.settings_version,
            self.settings_connection,
            self.settings_connection_policy,
            self.settings_connection_hint,
            self.settings_keepalive_interval,
            self.settings_keepalive_interval_hint,
            self.settings_keepalive_misses,
            self.settings_keepalive_misses_hint,
            self.settings_connect_timeout,
            self.settings_connect_timeout_hint,
            self.connection_summary,
            self.connection_window,
            self.connection_window_off,
            self.connection_window_hint,
            self.connection_defaults_note,
            self.host_connection_title,
            self.host_connection_inherit,
            self.host_connection_off,
            self.host_connection_override,
            self.host_connection_inherits,
            self.host_connection_inherits_off,
            self.host_connection_off_note,
            self.host_connection_override_note,
            self.import_connection,
            self.import_connection_off,
            self.import_connection_partial,
            self.connection_problem_missing,
            self.connection_problem_number,
            self.connection_problem_negative,
            self.connection_problem_interval,
            self.connection_problem_misses,
            self.connection_problem_timeout,
            self.connect_title,
            self.connect_hint,
            self.quick_hint,
            self.tool_close,
            self.connect_background,
            self.tool_forwards_title,
            self.tool_forwards_all,
            self.tool_forwards_host,
            self.tool_transfers_title,
            self.tool_transfers_subtitle,
            self.fwd_new,
            self.fwd_edit,
            self.fwd_type,
            self.fwd_kind_local,
            self.fwd_kind_remote,
            self.fwd_kind_dynamic,
            self.fwd_kind_local_hint,
            self.fwd_kind_remote_hint,
            self.fwd_kind_dynamic_hint,
            self.fwd_through,
            self.fwd_host_search,
            self.fwd_host_no_match,
            self.fwd_host_choose,
            self.fwd_listen_local,
            self.fwd_listen_remote,
            self.fwd_target_remote,
            self.fwd_target_local,
            self.fwd_preview_local,
            self.fwd_preview_remote,
            self.fwd_preview_dynamic,
            self.fwd_empty_title,
            self.fwd_empty_body,
            self.fwd_no_hosts,
            self.fwd_cancel,
            self.fwd_stop_title,
            self.fwd_stop_action,
            self.fwd_stop_body,
            self.fwd_delete_title,
            self.fwd_delete_action,
            self.fwd_delete_body,
            self.fwd_delete_running,
            self.fwd_delete_saved,
            self.fwd_here,
            self.fwd_there,
            self.fwd_failure_in_use,
            self.fwd_failure_denied,
            self.fwd_failure_server,
            self.fwd_failure_target,
            self.fwd_more,
            self.files_name,
            self.files_size,
            self.files_modified,
            self.files_this_computer,
            self.session_forwards,
            self.custom_shell_title,
            self.drop_upload_failed,
            self.drop_folders_unsupported,
            self.drop_uploading,
            self.toast_uploaded,
            self.toast_downloaded,
            self.toast_path_copied,
            self.files_open,
            self.files_copy_path,
            self.files_mark_row,
            self.files_unmark_row,
            self.drop_uploading_files,
            self.settings_scheme,
            self.settings_scheme_hint,
            self.settings_scheme_app,
            self.custom_shell_body,
            self.custom_shell_run,
            self.settings_keys,
            self.settings_keys_hint,
            self.settings_nav_hint,
            self.keys_change,
            self.keys_unbind,
            self.keys_reset,
            self.keys_reset_all,
            self.keys_unbound,
            self.keys_changed,
            self.keys_default,
            self.keys_capture_title,
            self.keys_capture_hint,
            self.keys_capture_cancel,
            self.keys_no_match,
            self.keys_custom_summary,
            self.keys_problem_hint,
            self.keys_save_failed,
            self.keys_save_failed_next,
            self.keys_scope_label,
            self.keys_scope_all,
            self.keys_conflict_next,
            self.snippet_manage_title,
            self.snippet_manage_hint,
            self.snippet_storage_note,
            self.snippet_new,
            self.snippet_edit_title,
            self.snippet_field_name,
            self.snippet_field_body,
            self.snippet_name_placeholder,
            self.snippet_body_placeholder,
            self.snippet_editor_keys,
            self.snippet_save,
            self.snippet_insert,
            self.snippet_run,
            self.snippet_fill_hint,
            self.snippet_run_hint,
            self.snippet_edit,
            self.snippet_delete,
            self.snippet_delete_title,
            self.snippet_delete_body,
            self.snippet_delete_yes,
            self.snippet_delete_no,
            self.snippet_empty_title,
            self.snippet_empty_body,
            self.snippet_no_terminal,
            self.snippet_no_terminal_body,
            self.snippet_terminal_ended,
            self.snippet_terminal_ended_body,
            self.snippet_inserted,
            self.snippet_sent,
            self.snippet_waiting,
            self.snippet_waiting_body,
            self.snippet_saved,
            self.snippet_deleted,
            self.snippet_unreadable_title,
            self.snippet_unreadable_body,
            self.snippet_save_failed_detail,
            self.snippet_line_count,
            self.snippet_count,
            self.snippet_manage_action,
            self.snippet_new_action,
            self.snippet_insert_action,
            self.snippet_run_action,
            self.snippet_problem_name_empty,
            self.snippet_problem_name_long,
            self.snippet_problem_body_empty,
            self.snippet_problem_body_long,
            self.snippet_problem_unknown,
            self.hosts_groups_title,
            self.hosts_connect_hint,
            self.host_example_group,
            self.copy_click_hint,
            self.toast_copied,
            self.connect_step_dial,
            self.connect_step_identity,
            self.connect_step_login,
            self.connect_step_terminal,
            self.connect_more_options,
            self.connect_change_sign_in,
            self.auth_auto,
            self.auth_auto_hint,
            self.host_dialog_intro,
            self.host_name_optional,
            self.host_group_optional,
            self.host_password_optional,
            self.host_user_optional,
            self.host_key_has_passphrase,
            self.host_advanced_title,
            self.host_advanced_hint,
            self.host_advanced_in_use,
            self.host_group_basic,
            self.host_group_route,
            self.host_group_transfer,
            self.host_group_options,
            self.host_jump_search,
            self.host_jump_none_left,
            self.host_jump_no_match,
            self.hosts_group_new,
            self.hosts_group_rename,
            self.hosts_group_move,
            self.hosts_group_delete,
            self.import_search_placeholder,
            self.import_count,
            self.import_row_limit,
            self.hosts_group_name_label,
            self.hosts_group_name_required,
            self.hosts_group_destination_hint,
            self.hosts_group_create_action,
            self.hosts_group_rename_action,
            self.hosts_move_ungrouped_action,
            self.hosts_move_none_row,
            self.hosts_move_title,
            self.hosts_group_move_action,
            self.hosts_group_delete_action,
            self.hosts_group_delete_body,
            self.hosts_group_created,
            self.hosts_group_renamed,
            self.hosts_group_deleted,
            self.hosts_moved,
            self.hosts_status_label,
            self.hosts_status_any,
            self.hosts_status_connected,
            self.hosts_status_recent,
            self.hosts_status_never,
            self.hosts_protocol_label,
            self.hosts_protocol_all,
            self.hosts_group_search_placeholder,
            self.hosts_group_no_match,
            self.hosts_group_search_clear,
            self.hosts_selection_ssh_only,
            self.hosts_group_picker_create,
            self.validation_port_not_number,
            self.validation_port_range,
            self.validation_address_port_conflict,
            self.validation_baud_not_number,
            self.validation_baud_range,
            self.validation_data_bits_not_number,
            self.validation_data_bits_range,
            self.validation_address_empty,
            self.validation_address_whitespace,
            self.validation_user_empty,
            self.validation_user_whitespace,
            self.validation_name_empty,
            self.validation_device_empty,
            self.validation_control_char,
            self.validation_group_invalid,
            self.hosts_scope_current,
            self.hosts_scope_all,
            self.hosts_selection_count,
            self.hosts_select_scope,
            self.hosts_select_none,
            self.hosts_select_start,
            self.hosts_filter_clear,
            self.hosts_no_match_hint,
            self.hosts_group_empty,
            self.hosts_batch_open,
            self.hosts_batch_open_hint,
            self.batch_title,
            self.batch_preview_subtitle,
            self.batch_targets_section,
            self.batch_targets_count,
            self.batch_scope_label,
            self.batch_targets_refused,
            self.batch_targets_more,
            self.batch_steps_section,
            self.batch_step_add,
            self.batch_step_remove,
            self.batch_step_placeholder,
            self.batch_concurrency,
            self.batch_concurrency_hint,
            self.batch_failure,
            self.batch_failure_skip,
            self.batch_failure_stop,
            self.batch_name,
            self.batch_name_placeholder,
            self.batch_confirm,
            self.batch_cancel,
            self.batch_close,
            self.batch_run_running,
            self.batch_run_paused,
            self.batch_run_cancelling,
            self.batch_run_cancelled,
            self.batch_run_completed,
            self.batch_run_failed,
            self.batch_run_interrupted,
            self.batch_run_progress,
            self.batch_run_pause,
            self.batch_run_resume,
            self.batch_run_cancel,
            self.batch_run_retry,
            self.batch_run_output,
            self.batch_run_output_empty,
            self.batch_run_output_dropped,
            self.batch_run_timeline,
            self.batch_run_step,
            self.batch_no_rollback,
            self.batch_retry_note,
            self.batch_trust_question,
            self.batch_trust_accept,
            self.batch_trust_decline,
            self.batch_proxy_question,
            self.batch_count_steps,
            self.batch_exit,
            self.batch_from_step,
            self.batch_no_targets,
            self.batch_reason_no_credential,
            self.batch_reason_missing_host,
            self.batch_save_failed,
            self.batch_log_failed,
            self.batch_switch_library,
            self.batch_switch_run,
            self.batch_switch_history,
            self.batch_switch_editor,
            self.batch_center_open,
            self.batch_center_open_hint,
            self.batch_library_title,
            self.batch_library_subtitle,
            self.batch_library_empty_title,
            self.batch_library_empty_body,
            self.batch_library_new,
            self.batch_library_edit,
            self.batch_library_delete,
            self.batch_library_delete_ask,
            self.batch_library_delete_yes,
            self.batch_library_delete_no,
            self.batch_library_deleted,
            self.batch_library_row_meta,
            self.batch_library_more,
            self.batch_untitled,
            self.batch_saved,
            self.batch_save_draft,
            self.batch_step_up,
            self.batch_step_down,
            self.batch_history_title,
            self.batch_history_subtitle,
            self.batch_history_empty,
            self.batch_history_filter,
            self.batch_history_filter_any,
            self.batch_history_omitted,
            self.batch_history_damaged,
            self.batch_history_open,
            self.batch_history_back,
            self.batch_history_no_host,
            self.batch_history_when,
            self.batch_history_hosts,
            self.batch_history_output_unread,
            self.batch_history_output_unread_reason,
            self.batch_diagnostics_copy,
            self.batch_diagnostics_copied,
            self.batch_run_background,
            self.batch_run_background_hint,
            self.batch_indicator_running,
            self.batch_indicator_failed,
            self.batch_run_save_failed,
            self.batch_retry_confirm,
            self.batch_step_pending,
            self.batch_step_running,
            self.batch_step_succeeded,
            self.batch_step_failed,
            self.batch_step_skipped,
            self.batch_step_cancelled,
            self.batch_step_already_done,
            self.batch_step_interrupted,
            self.batch_host_queued,
            self.batch_host_connecting,
            self.batch_host_running,
            self.batch_host_waiting,
            self.batch_host_succeeded,
            self.batch_host_failed,
            self.batch_host_cancelled,
            self.batch_host_skipped,
            self.batch_host_interrupted,
            self.field_clear,
            self.terminal_jump_bottom,
            self.terminal_jump_bottom_short,
            self.terminal_select_all,
            self.terminal_font_reset,
            self.host_copy_target,
            self.home_quick_placeholder,
            self.home_quick_temporary,
            self.home_quick_temporary_hint,
            self.home_quick_enter,
            self.home_enter_first_hint,
            self.home_recent_strip,
            self.home_recent_strip_hint,
            self.home_recent_strip_all,
            self.home_more_ways,
            self.home_more_import_hint,
            self.home_more_restore_hint,
            self.home_more_quick,
            self.home_more_quick_hint,
            self.home_local_button,
            self.home_serial_section,
            self.home_serial_new,
            self.home_favorites_section,
            self.home_card_more,
            self.home_sort_label,
            self.home_sort_saved,
            self.home_sort_name,
            self.home_sort_recent,
            self.home_filter,
            self.home_sidebar_collapse,
            self.home_sidebar_expand,
            self.home_favorites_empty,
            self.home_favorites_empty_hint,
            self.favorite_add,
            self.favorite_remove,
            self.favorite_add_named,
            self.favorite_remove_named,
            self.favorite_added,
            self.favorite_removed,
            self.terminal_more,
            self.terminal_grid_size,
            self.session_connecting_card_hint,
            self.banner_reconnecting,
            self.banner_cancel,
            self.banner_dismiss,
            self.banner_diagnostic,
            self.banner_copy_diagnostic,
            self.palette_host_jumps,
            self.palette_host_jumps_one,
            self.palette_host_proxy_command,
            self.hosts_group_empty_body,
            self.hosts_delete_selected,
            self.hosts_delete_selected_title,
            self.hosts_delete_selected_body,
            self.hosts_deleted_many,
            self.host_jump_keys,
            self.pane_zoomed,
            self.focus_mode_enter,
            self.focus_mode_exit,
            self.pane_divider_reset_hint,
            self.desktop_pan_hint,
            self.toast_dismiss,
            self.files_go_to_path,
            self.files_go_to_title,
            self.files_go_to_action,
            self.transfers_clear_finished,
            self.fwd_missing_host,
            self.batch_output_copy,
            self.batch_output_copied,
            self.batch_run_in_progress_refusal,
            self.batch_filter_all,
            self.batch_filter_failed,
            self.batch_filter_waiting,
            self.batch_filter_running,
            self.batch_filter_done,
            self.snippet_search_placeholder,
            self.snippet_search_empty,
            self.snippet_keys_hint,
            self.quick_address_placeholder,
            self.settings_reset_default,
            self.sync_unsaved_changes,
            self.host_move_to_group,
            self.hosts_move_single_title,
            self.host_open_files,
            self.hosts_group_new_host_here,
            self.hosts_group_new_subgroup,
            self.terminal_jump_bottom_new,
            self.terminal_open_link,
            self.terminal_copy_link,
            self.terminal_clear_scrollback,
            self.terminal_snippets_menu,
            self.snippet_palette_aliases,
            self.session_global_forwards,
        ]
    }
}

/// English.
pub static ENGLISH_UX: UxStrings = UxStrings {
    settings_title: "Settings",
    settings_summary: "Appearance, terminal, shortcuts and data",
    new_tab: "New tab",
    new_tab_menu: "New tab: a shell or a host",
    launcher_placeholder: "Search shells and hosts, or type user@host:port",
    launcher_connect: "Connect to {target}",
    search: "Search hosts and commands",
    home_local_section: "This computer",
    home_local_hint: "Opens in a new tab",
    home_hosts_section: "Servers",
    home_welcome_title: "Add your first server",
    home_welcome_body: "Enter the address, user and sign-in once, then connect with one click. Just trying a server? Type user@host into the search to connect without saving.",
    home_import: "Import from SSH config",
    terminals_empty_title: "No terminal is open",
    terminals_empty_body: "Open a shell on this computer or connect to a server from Hosts.",
    settings_appearance: "Appearance",
    settings_theme: "Theme",
    settings_theme_hint: "Follow the system, or stay light or dark",
    settings_language: "Language",
    settings_language_hint: "Applies at once, including inside terminals",
    settings_text_size: "Interface text size",
    settings_text_size_hint: "Scales every label, button and list",
    settings_motion: "Motion",
    settings_motion_hint: "Reduced skips short fades; the system's reduce-motion setting does too",
    settings_notices: "Background notices",
    settings_bell_notice: "Bell",
    settings_highlight: "Highlight output",
    settings_os_icons: "Operating system icons",
    settings_os_icons_hint: "Shows the system a host runs on its tile, read from the server when you connect; turned off, the tiles show letters and servers are not asked",
    settings_highlight_hint: "Colours IP addresses, links and words like error and warning in plain output; a program's own colours and full-screen programs are left as they are",
    settings_bell_notice_hint: "Marks the tab of a shell that rang while you were elsewhere; the bell byte still reaches the program",
    settings_finished_notice: "Command ended",
    settings_finished_notice_hint: "Marks the tab only when the shell itself reports the end; a shell without integration never marks one",
    notice_on: "On",
    notice_off: "Off",
    settings_terminal: "Terminal",
    settings_font_size: "Font size",
    settings_font_size_hint: "Used by new terminals and the open ones",
    settings_default_shell: "Default shell",
    settings_default_shell_hint: "What the + button opens",
    settings_data: "Data",
    settings_hosts_file: "Saved hosts",
    settings_import_hint: "Adds hosts from ~/.ssh/config after a preview",
    settings_version: "Version",
    settings_connection: "Connection",
    settings_connection_policy: "Keepalive and timeout",
    settings_connection_hint: "Every host is dialled with these numbers unless it sets its own. \
Nothing here is applied to a connection that is already open.",
    settings_keepalive_interval: "Keepalive interval",
    settings_keepalive_interval_hint: "A connection silent for this long sends one probe",
    settings_keepalive_misses: "Unanswered probes",
    settings_keepalive_misses_hint: "How many probes may go unanswered before the connection is \
dropped",
    settings_connect_timeout: "Connect timeout",
    settings_connect_timeout_hint: "How long reaching the host may take, DNS and authentication \
included",
    connection_summary: "A probe every {interval} s, dropped after {misses} unanswered, connect timeout {timeout} s",
    connection_window: "{seconds} s without a reply before the connection is treated as lost",
    connection_window_off: "Keepalive is off: Shellune will not notice a link that went silent",
    connection_window_hint: "One probe is sent per interval, and the link is dropped once the probes above have gone unanswered",
    connection_defaults_note: "Hosts that do not override these numbers are dialled with them. An interval of 0 turns keepalive off.",
    host_connection_title: "Keepalive",
    host_connection_inherit: "Inherit",
    host_connection_off: "Off",
    host_connection_override: "Override",
    host_connection_inherits: "Inherits the global default: {summary}",
    host_connection_inherits_off: "Inherits the global default, where keepalive is off for every host and a connection is given up after {timeout}s without a reply.",
    host_connection_off_note: "No keepalive probe is sent for this host, so a link that goes silent is not noticed. The connect timeout still follows the global default.",
    host_connection_override_note: "These numbers apply to this host only; the global default is unchanged.",
    import_connection: "Keepalive: a probe every {interval} s, dropped after {misses} unanswered",
    import_connection_off: "Keepalive: the file turns it off for this host",
    import_connection_partial: "Keepalive: the file sets only part of it, the rest follows the global default",
    connection_problem_missing: "Type a whole number; a keepalive interval of 0 turns the probe off",
    connection_problem_number: "{text} is not a whole number",
    connection_problem_negative: "{text} is below zero; use 0 to turn keepalive off",
    connection_problem_interval: "{value} s is above the {max} s Shellune keeps; 0 turns keepalive off",
    connection_problem_misses: "{value} unanswered probes is outside 1-{max}; 0 would never notice a dead link",
    connection_problem_timeout: "{value} s is outside 1-{max} for the connect timeout",
    connect_title: "Connect to",
    connect_hint: "Choose how this server checks it is you. A password is only kept if you tick Remember.",
    quick_hint: "Connect without saving. You can save the host afterwards from the session head.",
    tool_close: "Close",
    connect_background: "Continue in background",
    tool_forwards_title: "Port forwarding",
    tool_forwards_all: "Every saved tunnel",
    tool_forwards_host: "Tunnels through",
    tool_transfers_title: "Transfers",
    tool_transfers_subtitle: "Uploads and downloads of every connection",
    fwd_new: "New tunnel",
    fwd_edit: "Edit tunnel",
    fwd_type: "Type",
    fwd_kind_local: "Local",
    fwd_kind_remote: "Remote",
    fwd_kind_dynamic: "SOCKS proxy",
    fwd_kind_local_hint: "Reach a service the server can see (a database, an admin page) from this computer.",
    fwd_kind_remote_hint: "Let the server reach a service running on this computer.",
    fwd_kind_dynamic_hint: "Send apps through the server: set their SOCKS proxy to the local port.",
    fwd_through: "Through host",
    fwd_host_search: "Search hosts by name or address",
    fwd_host_no_match: "No saved host matches this search.",
    fwd_host_choose: "Choose a host",
    fwd_listen_local: "On this computer",
    fwd_listen_remote: "On the server",
    fwd_target_remote: "Destination, as the server sees it",
    fwd_target_local: "Destination on this computer",
    fwd_preview_local: "Opening {listen} on this computer reaches {target} through {host}.",
    fwd_preview_remote: "Opening {listen} on {host} reaches {target} on this computer.",
    fwd_preview_dynamic: "Apps whose SOCKS proxy is {listen} go out through {host}.",
    fwd_empty_title: "No port forwarding yet",
    fwd_empty_body: "A tunnel carries one port through an SSH host, so a service on one side can be used from the other.",
    fwd_no_hosts: "Save a host first: every tunnel runs through an SSH host.",
    fwd_cancel: "Cancel",
    fwd_stop_title: "Stop this tunnel?",
    fwd_stop_action: "Stop tunnel",
    fwd_stop_body: "{endpoint} stops listening now. The rule stays saved and can be started again; nothing on either machine is removed.",
    fwd_delete_title: "Delete this tunnel?",
    fwd_delete_action: "Delete rule",
    fwd_delete_body: "{rule} is removed from the saved rules. {running}",
    fwd_delete_running: "It is listening now, so it is stopped first.",
    fwd_delete_saved: "It is not listening.",
    fwd_here: "This computer",
    fwd_there: "Server",
    fwd_failure_in_use: "Port {port} on {host} is already in use by another program. The tunnel was not started; pick another port, or stop what holds this one.",
    fwd_failure_denied: "This account may not listen on {host}:{port}. The tunnel was not started.",
    fwd_failure_server: "The server refused this tunnel: {detail}",
    fwd_failure_target: "Nothing accepted a connection at {host}:{port}: {detail}",
    fwd_more: "Duplicate",
    files_name: "Name",
    files_size: "Size",
    files_modified: "Modified",
    files_this_computer: "This computer",
    session_forwards: "Ports",
    custom_shell_title: "Open a program",
    drop_upload_failed: "Could not upload to the server:",
    drop_folders_unsupported: "Only files can be uploaded by dropping; folders are not copied yet.",
    drop_uploading: "Uploading {count} file(s); the path is typed at the prompt",
    toast_uploaded: "Uploaded",
    toast_downloaded: "Downloaded",
    toast_path_copied: "Path copied",
    files_open: "Open",
    files_copy_path: "Copy path",
    files_mark_row: "Add this row to the selection",
    files_unmark_row: "Take this row out of the selection",
    drop_uploading_files: "Uploading {count} file(s)",
    settings_scheme: "Terminal colours",
    settings_scheme_hint: "Many command-line programs are made for a dark terminal. Text too close to its background is always made legible.",
    settings_scheme_app: "Match app",
    custom_shell_body: "Any program on this computer that runs in a terminal, by name or full path. It opens in a new tab.",
    custom_shell_run: "Open",
    settings_keys: "Keyboard shortcuts",
    settings_keys_hint: "Every action the window offers. Unbinding one hands its key back to the terminal program. ↑↓ pick an action, ←→ pick Change, Unbind or Restore, Enter runs it. Esc moves to the categories.",
    settings_nav_hint: "↑↓ choose a category, ←→ choose a setting, Enter opens it, Esc returns to the page.",
    keys_change: "Change",
    keys_unbind: "Unbind",
    keys_reset: "Restore",
    keys_reset_all: "Restore every shortcut",
    keys_unbound: "Unbound",
    keys_changed: "Changed",
    keys_default: "Default",
    keys_capture_title: "Press the new shortcut",
    keys_capture_hint: "Esc cancels. The combination needs Ctrl, Alt or ⌘ so the terminal keeps its own keys.",
    keys_capture_cancel: "Cancel",
    keys_no_match: "No action matches that search",
    keys_custom_summary: "{} changed",
    keys_problem_hint: "Saved shortcuts were not applied; every action is at its default.",
    keys_save_failed: "This shortcut works for now but could not be written to the preferences file.",
    keys_save_failed_next: "The next change tries again; a restart shows what was saved.",
    keys_scope_label: "Scope",
    keys_scope_all: "Every scope",
    keys_conflict_next: "Change or unbind one of the two, so one combination answers one action.",
    snippet_manage_title: "Shortcut commands",
    snippet_manage_hint: "Browsing and editing change nothing on screen. Fill in writes the command and runs nothing; Run writes it and submits it once, after a confirmation.",
    snippet_storage_note: "Saved as plain text in snippets.json on this computer. Keep passwords, keys and tokens out of shortcut commands.",
    snippet_new: "New shortcut command",
    snippet_edit_title: "Edit shortcut command",
    snippet_field_name: "Name",
    snippet_field_body: "Command",
    snippet_name_placeholder: "Deploy the service",
    snippet_body_placeholder: "make deploy && obsctl status",
    snippet_editor_keys: "Enter saves. Shift+Enter starts a new line, shown as \\n in the field.",
    snippet_save: "Save command",
    snippet_insert: "Fill in",
    snippet_run: "Run",
    snippet_fill_hint: "Writes the command into the terminal and runs nothing",
    snippet_run_hint: "Writes the command and submits it once, after a confirmation",
    snippet_edit: "Edit",
    snippet_delete: "Delete",
    snippet_delete_title: "Delete this shortcut command?",
    snippet_delete_body: "“{name}” is removed from this computer. Nothing already in a terminal changes.",
    snippet_delete_yes: "Delete it",
    snippet_delete_no: "Keep it",
    snippet_empty_title: "No shortcut commands yet",
    snippet_empty_body: "Save a command you run often, then insert it from the command palette.",
    snippet_no_terminal: "Open a terminal first",
    snippet_no_terminal_body: "Inserting and running need an active terminal. Open a local shell or connect a server, then choose the command again.",
    snippet_terminal_ended: "This terminal has no shell running",
    snippet_terminal_ended_body: "Nothing was written. Start a shell in this tab, or open another terminal, then choose the command again.",
    snippet_inserted: "Filled in; nothing was run",
    snippet_sent: "Ran once in the terminal",
    snippet_waiting: "Waiting in the paste dialog",
    snippet_waiting_body: "Nothing is written yet. Confirm it in the dialog; choosing “{cancel}” or pressing Esc writes nothing.",
    snippet_saved: "Shortcut command saved",
    snippet_deleted: "Shortcut command deleted",
    snippet_unreadable_title: "The saved commands could not be read",
    snippet_unreadable_body: "The file {path} is left exactly as it is and is never written over. Repair or remove it, then reopen Shellune.",
    snippet_save_failed_detail: "Nothing was saved: {reason}",
    snippet_line_count: "{count} lines",
    snippet_count: "{count} saved",
    snippet_manage_action: "Shortcut commands…",
    snippet_new_action: "New shortcut command…",
    snippet_insert_action: "Fill in “{name}”",
    snippet_run_action: "Run “{name}”",
    snippet_problem_name_empty: "A shortcut command needs a name.",
    snippet_problem_name_long: "The name is limited to {max} characters.",
    snippet_problem_body_empty: "A shortcut command needs a command body; nothing would be inserted or run.",
    snippet_problem_body_long: "The command body is limited to {max} characters.",
    snippet_problem_unknown: "That command is no longer in the library; reopen the list.",
    hosts_groups_title: "Groups",
    hosts_connect_hint: "Connect (or double-click the row)",
    host_example_group: "Production/Web",
    copy_click_hint: "Click to copy",
    toast_copied: "Copied",
    connect_step_dial: "Reach the server",
    connect_step_identity: "Verify the server",
    connect_step_login: "Sign in",
    connect_step_terminal: "Open the terminal",
    connect_more_options: "More options…",
    connect_change_sign_in: "Change how to sign in…",
    auth_auto: "Automatic",
    host_dialog_intro: "An address and a user are enough; everything else can wait.",
    host_name_optional: "Name (optional)",
    host_group_optional: "Group (optional)",
    host_password_optional: "Password (optional)",
    host_user_optional: "User (optional)",
    host_key_has_passphrase: "This key has a passphrase",
    host_advanced_title: "Advanced",
    host_advanced_hint: "Jump hosts, ProxyCommand, startup command, file transfer, X11, keepalive",
    host_advanced_in_use: "In use",
    host_group_basic: "Basics",
    host_group_route: "Jump hosts and proxy",
    host_group_transfer: "File transfer",
    host_group_options: "Options",
    host_jump_search: "Search by name or address",
    host_jump_none_left: "Every other saved SSH host is already in the chain.",
    host_jump_no_match: "No saved SSH host matches this search.",
    auth_auto_hint: "Tries the system SSH agent, then your keys in ~/.ssh (id_ed25519, id_ecdsa, id_rsa), then the password below if you give one.",
    hosts_group_new: "New group",
    hosts_group_rename: "Rename group",
    hosts_group_move: "Move to group…",
    hosts_group_delete: "Delete group",
    import_search_placeholder: "Search the hosts this file describes",
    import_count: "{shown} of {total} host(s)",
    import_row_limit: "Showing the first {shown} of {total} rows; use the search to narrow them.",
    hosts_group_name_label: "Group name",
    hosts_group_name_required: "Type a group name first.",
    hosts_group_destination_hint: "Search groups, or type a new one",
    hosts_group_create_action: "Create group",
    hosts_group_rename_action: "Rename",
    hosts_group_move_action: "Move to {name}",
    hosts_move_ungrouped_action: "Remove from group",
    hosts_move_none_row: "No group",
    hosts_move_title: "Move {count} host(s)",
    hosts_group_delete_action: "Delete group, keep hosts",
    hosts_group_delete_body: "Its {count} connection(s) move to the group chosen below; none is deleted.",
    hosts_group_created: "Group created",
    hosts_group_renamed: "Group renamed; {count} connection(s) moved with it",
    hosts_group_deleted: "Group deleted; {count} connection(s) kept",
    hosts_moved: "{count} host(s) moved",
    hosts_status_label: "Status",
    hosts_status_any: "Any status",
    hosts_status_connected: "Connected now",
    hosts_status_recent: "Reached before",
    hosts_status_never: "Never reached",
    hosts_protocol_label: "Protocol",
    hosts_protocol_all: "All protocols",
    hosts_group_search_placeholder: "Filter groups…",
    hosts_group_no_match: "No matching groups",
    hosts_group_search_clear: "Clear",
    hosts_selection_ssh_only: "SSH hosts only",
    hosts_group_picker_create: "New group “{name}”",
    validation_port_not_number: "{text} is not a port number; use 1-65535",
    validation_port_range: "{raw} is outside the valid range 1-65535",
    validation_address_port_conflict: "The address names port {typed} but the port field says {field}; keep one of them",
    validation_baud_not_number: "{text} is not a baud rate; use 50-4000000",
    validation_baud_range: "{value} is outside the range 50-4000000 this build accepts",
    validation_data_bits_not_number: "{text} is not a number of data bits; use 5-8",
    validation_data_bits_range: "{value} is outside the range 5-8 this build accepts",
    validation_address_empty: "Address is required",
    validation_address_whitespace: "Address cannot contain whitespace",
    validation_user_empty: "User is required",
    validation_user_whitespace: "User cannot contain whitespace",
    validation_name_empty: "Name is required",
    validation_device_empty: "Serial device is required",
    validation_control_char: "Cannot contain control characters",
    validation_group_invalid: "Group path is invalid",
    hosts_scope_current: "Current results",
    hosts_scope_all: "All hosts",
    hosts_selection_count: "{count} selected",
    hosts_select_scope: "Select all",
    hosts_select_none: "Clear",
    hosts_select_start: "Select",
    hosts_filter_clear: "Clear filters",
    hosts_no_match_hint: "Change the search or drop a filter to see more hosts.",
    hosts_group_empty: "No groups yet. Use + to make one.",
    hosts_batch_open: "Batch task",
    hosts_batch_open_hint: "Run ordered commands on the selected hosts, after you confirm the list",
    batch_title: "Batch task",
    batch_preview_subtitle: "Nothing connects until you confirm: this is the list, the commands and the limits a run would use.",
    batch_targets_section: "Targets",
    batch_targets_count: "{count} hosts will run these commands",
    batch_scope_label: "Targets from",
    batch_targets_refused: "Not included",
    batch_targets_more: "and {count} more, not listed",
    batch_steps_section: "Commands, in order",
    batch_step_add: "Add step",
    batch_step_remove: "Remove step",
    batch_step_placeholder: "Command to run on every host",
    batch_concurrency: "Hosts at once",
    batch_concurrency_hint: "1 to {max}, counted per connection",
    batch_failure: "When a host fails",
    batch_failure_skip: "Skip that host, keep the others",
    batch_failure_stop: "Stop scheduling new hosts",
    batch_name: "Task name (optional)",
    batch_name_placeholder: "Nightly check",
    batch_confirm: "Confirm and run",
    batch_cancel: "Cancel",
    batch_close: "Close",
    batch_run_running: "Running",
    batch_run_paused: "Paused: no new step starts, and a command that is running goes on",
    batch_run_cancelling: "Cancelling: stop was asked for, and the remote command has not ended yet",
    batch_run_cancelled: "Cancelled",
    batch_run_completed: "Completed",
    batch_run_failed: "Finished with failures",
    batch_run_interrupted: "Interrupted by a restart",
    batch_run_progress: "{done} of {total} hosts finished · {running} running · {concurrency} at once",
    batch_run_pause: "Pause",
    batch_run_resume: "Resume",
    batch_run_cancel: "Cancel run",
    batch_run_retry: "Retry failed hosts",
    batch_run_output: "Output",
    batch_run_output_empty: "This host has printed nothing yet.",
    batch_run_output_dropped: "{count} earlier lines are not kept on screen",
    batch_run_timeline: "Timeline",
    batch_run_step: "Step {index}",
    batch_no_rollback: "A retry runs the work still owed; nothing that already ran is undone.",
    batch_retry_note: "Retry: only the hosts and steps that did not succeed.",
    batch_trust_question: "The host presented {algorithm} {fingerprint}. Accept it for this run?",
    batch_trust_accept: "Accept",
    batch_trust_decline: "Decline",
    batch_proxy_question: "A ProxyCommand would run on this machine: {command}",
    batch_count_steps: "{count} steps",
    batch_exit: "exit {code}",
    batch_from_step: "starts at step {step}",
    batch_no_targets: "Select hosts, or choose a scope that covers some, before confirming.",
    batch_reason_no_credential: "{host} has no saved sign-in; add one on the host form",
    batch_reason_missing_host: "the saved host is gone",
    batch_save_failed: "The task definition was not saved",
    batch_log_failed: "The run log could not be written",
    batch_switch_library: "Saved tasks",
    batch_switch_run: "This run",
    batch_switch_history: "History",
    batch_switch_editor: "Being edited",
    batch_center_open: "Batch tasks",
    batch_center_open_hint: "Edit a task, watch the run this window is driving, or read past runs",
    batch_library_title: "Saved tasks",
    batch_library_subtitle: "Opening or editing one starts nothing: only a confirmation starts a run.",
    batch_library_empty_title: "No saved task yet",
    batch_library_empty_body: "Select hosts on the host page and open the batch editor; Save keeps the definition, and a confirmation runs it.",
    batch_library_new: "New task",
    batch_library_edit: "Edit",
    batch_library_delete: "Delete",
    batch_library_delete_ask: "Delete this task? Its past runs stay in the history.",
    batch_library_delete_yes: "Delete it",
    batch_library_delete_no: "Keep it",
    batch_library_deleted: "Task deleted",
    batch_library_row_meta: "{hosts} hosts · {steps} steps · {concurrency} at once",
    batch_library_more: "and {count} more task(s), not listed",
    batch_untitled: "Untitled task",
    batch_saved: "Saved; nothing was run",
    batch_save_draft: "Save task",
    batch_step_up: "Move this step earlier",
    batch_step_down: "Move this step later",
    batch_history_title: "Run history",
    batch_history_subtitle: "Summaries kept on this computer. A run cut off by an exit is marked interrupted, never replayed.",
    batch_history_empty: "No stored run yet",
    batch_history_filter: "Status",
    batch_history_filter_any: "Every status",
    batch_history_omitted: "{count} older runs are not listed",
    batch_history_damaged: "This record could not be read",
    batch_history_open: "Details",
    batch_history_back: "All runs",
    batch_history_no_host: "Choose a host to read its output and timeline.",
    batch_history_when: "Started {time}",
    batch_history_hosts: "{done}/{total} hosts finished",
    batch_history_output_unread: "This host's output was not read.",
    batch_history_output_unread_reason: "This host's output could not be read: {reason}",
    batch_diagnostics_copy: "Copy diagnostics",
    batch_diagnostics_copied: "Diagnostics copied; no credential is in them",
    batch_run_background: "Keep running in background",
    batch_run_background_hint: "The run keeps going; the indicator in the top bar brings this page back.",
    batch_indicator_running: "Batch {done}/{total}",
    batch_indicator_failed: "Batch: {failed} failed",
    batch_run_save_failed: "The run record was not written",
    batch_retry_confirm: "A retry runs only the hosts and steps that did not succeed; nothing that already ran is undone.",
    batch_step_pending: "waiting",
    batch_step_running: "running",
    batch_step_succeeded: "succeeded",
    batch_step_failed: "failed",
    batch_step_skipped: "skipped",
    batch_step_cancelled: "cancelled",
    batch_step_already_done: "done earlier",
    batch_step_interrupted: "interrupted",
    batch_host_queued: "waiting",
    batch_host_connecting: "connecting",
    batch_host_running: "running",
    batch_host_waiting: "needs your answer",
    batch_host_succeeded: "succeeded",
    batch_host_failed: "failed",
    batch_host_cancelled: "cancelled",
    batch_host_skipped: "skipped",
    batch_host_interrupted: "interrupted",
    field_clear: "Clear",
    terminal_jump_bottom: "Jump to bottom ({lines})",
    terminal_jump_bottom_short: "Jump to bottom",
    terminal_select_all: "Select all",
    terminal_font_reset: "Reset font size ({size}px)",
    host_copy_target: "Copy address",
    home_quick_placeholder: "Search saved hosts, or type user@host:port to connect",
    home_quick_temporary: "Connect to {target}",
    home_quick_temporary_hint: "Not saved · opens the sign-in",
    home_quick_enter: "Enter",
    home_enter_first_hint: "Enter connects to this host · ↑↓ to choose another",
    home_recent_strip: "Recent",
    home_recent_strip_hint: "Connect to {target} · {when}",
    home_recent_strip_all: "View all",
    home_more_ways: "More ways to start",
    home_more_import_hint: "Bring in the hosts of ~/.ssh/config",
    home_more_restore_hint: "Hosts, keys and settings from a backup file",
    home_more_quick: "Quick connect",
    home_more_quick_hint: "Type user@host in the search and press Enter",
    home_local_button: "Local terminal",
    home_serial_section: "Serial",
    home_serial_new: "New serial connection",
    home_favorites_section: "Favorites",
    home_card_more: "More actions",
    home_sort_label: "Sort",
    home_sort_saved: "Saved order",
    home_sort_name: "Name",
    home_sort_recent: "Last connected",
    home_filter: "Filter and sort",
    home_sidebar_collapse: "Collapse sidebar",
    home_sidebar_expand: "Expand sidebar",
    home_favorites_empty: "No favorites yet",
    home_favorites_empty_hint: "Star a host and it is kept here.",
    favorite_add: "Add to favorites",
    favorite_remove: "Remove from favorites",
    favorite_add_named: "Add {name} to favorites",
    favorite_remove_named: "Remove {name} from favorites",
    favorite_added: "Added to favorites",
    favorite_removed: "Removed from favorites",
    terminal_more: "More terminal tools",
    terminal_grid_size: "Terminal size",
    session_connecting_card_hint: "The terminal opens here as soon as the connection is up.",
    banner_reconnecting: "Reconnecting to {target}\u{2026}",
    banner_cancel: "Cancel",
    banner_dismiss: "Dismiss (Esc)",
    banner_diagnostic: "Diagnostics",
    banner_copy_diagnostic: "Copy diagnostics",
    palette_host_jumps: "{count} jumps",
    palette_host_jumps_one: "1 jump",
    palette_host_proxy_command: "ProxyCommand",
    hosts_group_empty_body: "This group has no saved connections yet.",
    hosts_delete_selected: "Delete selected",
    hosts_delete_selected_title: "Delete {count} selected hosts?",
    hosts_delete_selected_body: "These saved hosts will be removed from this computer. Active sessions stay open.",
    hosts_deleted_many: "Deleted {count} hosts",
    host_jump_keys: "↑↓ select · Enter add · Esc close",
    pane_zoomed: "Zoomed",
    focus_mode_enter: "Focus mode",
    focus_mode_exit: "Exit focus mode",
    pane_divider_reset_hint: "Drag to resize · Double-click to equalize",
    desktop_pan_hint: "Shift+scroll to pan",
    toast_dismiss: "Dismiss notification",
    files_go_to_path: "Go to folder\u{2026}",
    files_go_to_title: "Go to folder",
    files_go_to_action: "Open folder",
    transfers_clear_finished: "Clear finished",
    fwd_missing_host: "rule {rule}: host {host} is not saved, so this rule was not started",
    batch_output_copy: "Copy output",
    batch_output_copied: "Output copied",
    batch_run_in_progress_refusal: "A batch run is already active in this window; wait for it to finish or return to it.",
    batch_filter_all: "All",
    batch_filter_failed: "Failed",
    batch_filter_waiting: "Needs answer",
    batch_filter_running: "Running",
    batch_filter_done: "Succeeded",
    snippet_search_placeholder: "Filter shortcut commands\u{2026}",
    snippet_search_empty: "No matching shortcut commands.",
    snippet_keys_hint: "↑↓ select · Enter fill in · {run_key} run · Esc close",
    quick_address_placeholder: "example.com or 192.0.2.10",
    settings_reset_default: "Reset",
    sync_unsaved_changes: "Unsaved changes — click Save before syncing",
    host_move_to_group: "Move to group\u{2026}",
    hosts_move_single_title: "Move “{name}” to group",
    host_open_files: "Open files (SFTP)",
    hosts_group_new_host_here: "New host in this group",
    hosts_group_new_subgroup: "New subgroup",
    terminal_jump_bottom_new: "New output \u{00b7} Jump to bottom ({lines})",
    terminal_open_link: "Open link",
    terminal_copy_link: "Copy link",
    terminal_clear_scrollback: "Clear terminal & scrollback",
    terminal_snippets_menu: "Snippets\u{2026}",
    snippet_palette_aliases: "Snippets",
    session_global_forwards: "All port forwards\u{2026}",
};

/// Simplified Chinese.
pub static CHINESE_UX: UxStrings = UxStrings {
    settings_title: "设置",
    settings_summary: "外观、终端、快捷键与数据",
    new_tab: "新建标签",
    new_tab_menu: "新建标签：Shell 或主机",
    launcher_placeholder: "搜索 Shell 和主机，或输入 user@host:port",
    launcher_connect: "连接 {target}",
    search: "搜索主机与命令",
    home_local_section: "本机",
    home_local_hint: "在新标签中打开",
    home_hosts_section: "服务器",
    home_welcome_title: "添加你的第一台服务器",
    home_welcome_body: "填写一次地址、用户名和登录方式，以后一键连接。只是临时连一下？在搜索框输入 user@host 即可直接连接，不会保存。",
    home_import: "从 SSH 配置导入",
    terminals_empty_title: "没有打开的终端",
    terminals_empty_body: "在“主机”中打开本机终端或连接服务器。",
    settings_appearance: "外观",
    settings_theme: "主题",
    settings_theme_hint: "跟随系统，或固定为浅色 / 深色",
    settings_language: "语言",
    settings_language_hint: "立即生效，终端内也一样",
    settings_text_size: "界面文字大小",
    settings_text_size_hint: "缩放所有文字、按钮和列表",
    settings_motion: "动态效果",
    settings_motion_hint: "“减少”会跳过短暂的淡入效果；系统开启减少动态效果时同样跳过",
    settings_notices: "后台提醒",
    settings_bell_notice: "响铃",
    settings_highlight: "高亮输出内容",
    settings_os_icons: "操作系统图标",
    settings_os_icons_hint: "在主机图标上显示它运行的操作系统，连接时向服务器读取；关闭后图标显示字母，也不再向服务器读取",
    settings_highlight_hint: "为普通输出中的 IP 地址、链接以及 error、warning 等字样着色；程序自己设置的颜色和全屏程序（如 vim、htop）保持不变",
    settings_bell_notice_hint: "你在别处时响铃的 Shell，其标签会被标记；响铃字节仍照常送到程序",
    settings_finished_notice: "命令结束",
    settings_finished_notice_hint: "只有 Shell 自己报告命令结束时才标记标签；没有 Shell 集成不会产生该标记",
    notice_on: "开",
    notice_off: "关",
    settings_terminal: "终端",
    settings_font_size: "字号",
    settings_font_size_hint: "新终端和已打开的终端都会使用",
    settings_default_shell: "默认 Shell",
    settings_default_shell_hint: "点击 + 时打开的 Shell",
    settings_data: "数据",
    settings_hosts_file: "主机文件",
    settings_import_hint: "预览后从 ~/.ssh/config 添加主机",
    settings_version: "版本",
    settings_connection: "连接",
    settings_connection_policy: "保活与超时",
    settings_connection_hint: "没有单独设置的主机都按这里的数字连接。改动不会影响已经打开的连接。",
    settings_keepalive_interval: "保活间隔",
    settings_keepalive_interval_hint: "连接静默这么久之后就发一次探测",
    settings_keepalive_misses: "未响应探测",
    settings_keepalive_misses_hint: "多少次探测没有回应就判定连接已断开",
    settings_connect_timeout: "连接超时",
    settings_connect_timeout_hint: "连到主机最多可以用多久，包含 DNS 与认证",
    connection_summary: "每 {interval} 秒探测一次，{misses} 次无回应即判定断开，连接超时 {timeout} 秒",
    connection_window: "{seconds} 秒没有任何回应就判定连接断开",
    connection_window_off: "保活已关闭：Shellune 不会主动发现静默断开的连接",
    connection_window_hint: "每个间隔发一次探测，上面的次数都用完仍无回应就判定断开",
    connection_defaults_note: "没有单独设置的主机都按这里的数字连接。间隔填 0 表示关闭保活。",
    host_connection_title: "保活",
    host_connection_inherit: "继承全局",
    host_connection_off: "禁用",
    host_connection_override: "覆盖",
    host_connection_inherits: "继承全局默认：{summary}",
    host_connection_inherits_off: "继承全局默认：所有主机都关闭保活，连接在 {timeout} 秒没有响应后放弃。",
    host_connection_off_note: "本主机不发送保活探测，因此静默断开的连接不会被发现。连接超时仍沿用全局默认。",
    host_connection_override_note: "这些数字只用于本主机，全局默认不受影响。",
    import_connection: "保活：每 {interval} 秒探测一次，{misses} 次无回应即断开",
    import_connection_off: "保活：文件对本主机关闭了保活",
    import_connection_partial: "保活：文件只设置了其中一部分，其余沿用全局默认",
    connection_problem_missing: "请填写整数；保活间隔填 0 表示关闭探测",
    connection_problem_number: "{text} 不是整数",
    connection_problem_negative: "{text} 是负数；想关闭保活请填 0",
    connection_problem_interval: "{value} 秒超过了 Shellune 支持的 {max} 秒；想关闭保活请填 0",
    connection_problem_misses: "{value} 次不在 1-{max} 之间；填 0 就永远不会判定断开",
    connection_problem_timeout: "{value} 秒不在连接超时允许的 1-{max} 秒之间",
    connect_title: "连接到",
    connect_hint: "选择服务器验证你身份的方式。只有勾选“记住”时才会保存密码。",
    quick_hint: "不保存直接连接。连接后可以在会话头部保存为主机。",
    tool_close: "关闭",
    connect_background: "转到后台",
    tool_forwards_title: "端口转发",
    tool_forwards_all: "所有已保存的端口转发",
    tool_forwards_host: "经由此主机的端口转发：",
    tool_transfers_title: "传输",
    tool_transfers_subtitle: "所有连接的上传与下载",
    fwd_new: "新建端口转发",
    fwd_edit: "编辑端口转发",
    fwd_type: "类型",
    fwd_kind_local: "本地转发",
    fwd_kind_remote: "远程转发",
    fwd_kind_dynamic: "SOCKS 代理",
    fwd_kind_local_hint: "在本机访问服务器能访问到的服务，例如数据库、管理后台。",
    fwd_kind_remote_hint: "让服务器访问本机上运行的服务。",
    fwd_kind_dynamic_hint: "让应用经由服务器上网：把应用的 SOCKS 代理设置为本机这个端口。",
    fwd_through: "经由主机",
    fwd_host_search: "按名称或地址搜索主机",
    fwd_host_no_match: "没有与搜索匹配的已保存主机。",
    fwd_host_choose: "选择主机",
    fwd_listen_local: "本机监听",
    fwd_listen_remote: "服务器监听",
    fwd_target_remote: "目标（从服务器看）",
    fwd_target_local: "目标（本机上）",
    fwd_preview_local: "在本机访问 {listen}，即经由 {host} 访问 {target}。",
    fwd_preview_remote: "在 {host} 上访问 {listen}，即访问本机的 {target}。",
    fwd_preview_dynamic: "SOCKS 代理设为 {listen} 的应用会经由 {host} 访问网络。",
    fwd_empty_title: "还没有端口转发",
    fwd_empty_body: "端口转发经由一台 SSH 主机传递一个端口，让一侧的服务可以在另一侧使用。",
    fwd_no_hosts: "请先保存一台主机：每条端口转发都要经由 SSH 主机。",
    fwd_cancel: "取消",
    fwd_stop_title: "停止这条隧道？",
    fwd_stop_action: "停止隧道",
    fwd_stop_body: "{endpoint} 会立即停止监听。规则仍然保存，可以再次启动；两侧机器上都不会删除任何内容。",
    fwd_delete_title: "删除这条隧道？",
    fwd_delete_action: "删除规则",
    fwd_delete_body: "{rule} 会从已保存的规则中移除。{running}",
    fwd_delete_running: "它现在正在监听，会先被停止。",
    fwd_delete_saved: "它当前没有监听。",
    fwd_here: "本机",
    fwd_there: "服务器",
    fwd_failure_in_use: "端口 {port}（{host}）已被其它程序占用，这条隧道没有启动；请换一个端口，或先停止占用它的程序。",
    fwd_failure_denied: "没有权限在 {host}:{port} 上监听，这条隧道没有启动。",
    fwd_failure_server: "服务器拒绝了这条隧道：{detail}",
    fwd_failure_target: "无法连接到 {host}:{port}：{detail}",
    fwd_more: "复制一份",
    files_name: "名称",
    files_size: "大小",
    files_modified: "修改时间",
    files_this_computer: "本机",
    session_forwards: "端口转发",
    custom_shell_title: "打开程序",
    drop_upload_failed: "无法上传到服务器：",
    drop_folders_unsupported: "拖放上传只支持文件，暂不支持复制文件夹。",
    drop_uploading: "正在上传 {count} 个文件，路径已填入命令行",
    toast_uploaded: "上传完成",
    toast_downloaded: "下载完成",
    toast_path_copied: "已复制路径",
    files_open: "打开",
    files_copy_path: "复制路径",
    files_mark_row: "把这一行加入选择",
    files_unmark_row: "把这一行移出选择",
    drop_uploading_files: "正在上传 {count} 个文件",
    settings_scheme: "终端配色",
    settings_scheme_hint: "很多命令行程序按深色终端设计。与背景过于接近的文字始终会自动调整到可读。",
    settings_scheme_app: "跟随应用",
    custom_shell_body: "本机上任何可以在终端里运行的程序，填名称或完整路径，会在新标签里打开。",
    custom_shell_run: "打开",
    settings_keys: "键盘快捷键",
    settings_keys_hint: "窗口提供的全部动作。解除绑定后，该组合键会交还给终端程序。↑↓ 选择动作，←→ 选择修改、解除绑定或恢复默认，Enter 执行；Esc 回到分类。",
    settings_nav_hint: "↑↓ 选择分类，←→ 选择设置项，Enter 打开，Esc 返回当前页。",
    keys_change: "修改",
    keys_unbind: "解除绑定",
    keys_reset: "恢复默认",
    keys_reset_all: "恢复全部快捷键",
    keys_unbound: "未绑定",
    keys_changed: "已修改",
    keys_default: "默认",
    keys_capture_title: "按下新的组合键",
    keys_capture_hint: "Esc 取消。组合键需要带上 Ctrl、Alt 或 ⌘，否则终端就收不到自己的按键。",
    keys_capture_cancel: "取消",
    keys_no_match: "没有匹配的动作",
    keys_custom_summary: "已修改 {} 项",
    keys_problem_hint: "保存的快捷键未生效；所有动作已回到默认键位。",
    keys_save_failed: "快捷键本次已生效，但没能写入偏好文件。",
    keys_save_failed_next: "再改一次会重试；重启后显示的是已保存的状态。",
    keys_scope_label: "作用范围",
    keys_scope_all: "全部范围",
    keys_conflict_next: "改掉或解除其中一个，让每个组合键只对应一个动作。",
    snippet_manage_title: "快捷命令",
    snippet_manage_hint: "浏览和编辑不会改动终端。「填入」只把命令写进当前终端，不执行；「执行」写入后提交一次，且先要确认。",
    snippet_storage_note: "以明文保存在本机 snippets.json 中。不要在快捷命令里写密码、私钥或令牌。",
    snippet_new: "新建快捷命令",
    snippet_edit_title: "编辑快捷命令",
    snippet_field_name: "名称",
    snippet_field_body: "命令正文",
    snippet_name_placeholder: "部署服务",
    snippet_body_placeholder: "例如 make deploy && obsctl status",
    snippet_editor_keys: "回车保存；Shift+回车换行，换行在字段里显示为 \\n。",
    snippet_save: "保存命令",
    snippet_insert: "填入",
    snippet_run: "执行",
    snippet_fill_hint: "只把命令写进终端，不执行",
    snippet_run_hint: "写入命令后提交一次，提交前需要确认",
    snippet_edit: "编辑",
    snippet_delete: "删除",
    snippet_delete_title: "删除这条快捷命令？",
    snippet_delete_body: "「{name}」会从本机删除；终端里已有的内容不受影响。",
    snippet_delete_yes: "删除",
    snippet_delete_no: "保留",
    snippet_empty_title: "还没有快捷命令",
    snippet_empty_body: "把经常重复的命令存下来，之后从命令面板插入。",
    snippet_no_terminal: "请先打开终端",
    snippet_no_terminal_body: "插入和执行都需要当前终端。先打开本地 Shell 或连接主机，再选这条命令。",
    snippet_terminal_ended: "这条终端没有正在运行的 Shell",
    snippet_terminal_ended_body: "没有写入任何内容。在这个标签里重新启动 Shell，或换一个终端，再选这条命令。",
    snippet_inserted: "已填入，未执行",
    snippet_sent: "已在终端执行一次",
    snippet_waiting: "已在粘贴弹窗中等待确认",
    snippet_waiting_body: "还没有写入。请在弹窗里确认；选「{cancel}」或按 Esc 不会写入任何内容。",
    snippet_saved: "已保存快捷命令",
    snippet_deleted: "已删除快捷命令",
    snippet_unreadable_title: "快捷命令文件读取失败",
    snippet_unreadable_body: "文件 {path} 保持原样，不会被覆盖。修复或删除它，然后重新打开 Shellune。",
    snippet_save_failed_detail: "没有保存：{reason}",
    snippet_line_count: "{count} 行",
    snippet_count: "已保存 {count} 条",
    snippet_manage_action: "快捷命令…",
    snippet_new_action: "新建快捷命令…",
    snippet_insert_action: "填入「{name}」",
    snippet_run_action: "执行「{name}」",
    snippet_problem_name_empty: "快捷命令需要名称。",
    snippet_problem_name_long: "名称最多 {max} 个字符。",
    snippet_problem_body_empty: "快捷命令需要命令正文，否则插入或执行都是空的。",
    snippet_problem_body_long: "命令正文最多 {max} 个字符。",
    snippet_problem_unknown: "这条命令已不在库中，请重新打开列表。",
    hosts_groups_title: "分组",
    hosts_connect_hint: "连接（也可以双击这一行）",
    host_example_group: "生产/Web",
    copy_click_hint: "点击复制",
    toast_copied: "已复制",
    connect_step_dial: "连接服务器",
    connect_step_identity: "验证服务器身份",
    connect_step_login: "登录",
    connect_step_terminal: "打开终端",
    connect_more_options: "更多选项…",
    connect_change_sign_in: "修改登录方式…",
    auth_auto: "自动",
    host_dialog_intro: "填好地址和用户名就能连接，其余都可以以后再说。",
    host_name_optional: "名称（可选）",
    host_group_optional: "分组（可选）",
    host_password_optional: "密码（可选）",
    host_user_optional: "用户名（可选）",
    host_key_has_passphrase: "这个私钥设置了口令",
    host_advanced_title: "高级设置",
    host_advanced_hint: "跳板机、代理命令、启动命令、文件传输、X11、保活",
    host_advanced_in_use: "已设置",
    host_group_basic: "基本信息",
    host_group_route: "跳板与代理",
    host_group_transfer: "文件传输",
    host_group_options: "选项",
    host_jump_search: "按名称或地址搜索",
    host_jump_none_left: "其余已保存的 SSH 主机都已在跳板链中。",
    host_jump_no_match: "没有与搜索匹配的 SSH 主机。",
    auth_auto_hint: "依次尝试系统 SSH Agent、~/.ssh 里的私钥（id_ed25519、id_ecdsa、id_rsa），最后用下面的密码（可不填）。",
    hosts_group_new: "新建分组",
    hosts_group_rename: "重命名分组",
    hosts_group_move: "移动到分组…",
    hosts_group_delete: "删除分组",
    import_search_placeholder: "在这个文件描述的主机里搜索",
    import_count: "共 {total} 台，当前显示 {shown} 台",
    import_row_limit: "只绘制前 {shown} 行（共 {total} 行），用上面的搜索收窄。",
    hosts_group_name_label: "分组名称",
    hosts_group_name_required: "请先填写分组名称。",
    hosts_group_destination_hint: "搜索分组，或输入新分组名",
    hosts_group_create_action: "创建分组",
    hosts_group_rename_action: "重命名",
    hosts_group_move_action: "移动到 {name}",
    hosts_move_ungrouped_action: "移出分组",
    hosts_move_none_row: "不属于任何分组",
    hosts_move_title: "移动 {count} 台主机",
    hosts_group_delete_action: "删除分组并保留主机",
    hosts_group_delete_body: "其中 {count} 个连接会移到下面选中的分组，不会删除任何连接。",
    hosts_group_created: "已创建分组",
    hosts_group_renamed: "已重命名分组，{count} 个连接一起移动",
    hosts_group_deleted: "已删除分组，{count} 个连接保留",
    hosts_moved: "已移动 {count} 台主机",
    hosts_status_label: "状态",
    hosts_status_any: "所有状态",
    hosts_status_connected: "当前已连接",
    hosts_status_recent: "曾经连接过",
    hosts_status_never: "从未连接",
    hosts_protocol_label: "协议",
    hosts_protocol_all: "全部协议",
    hosts_group_search_placeholder: "定位分组…",
    hosts_group_no_match: "没有匹配的分组",
    hosts_group_search_clear: "清除",
    hosts_selection_ssh_only: "仅限 SSH 主机",
    hosts_group_picker_create: "新建分组“{name}”",
    validation_port_not_number: "{text} 不是有效端口号，请填写 1-65535",
    validation_port_range: "{raw} 超出有效端口范围 1-65535",
    validation_address_port_conflict: "地址中指定了端口 {typed}，但端口栏为 {field}，请只保留一个",
    validation_baud_not_number: "{text} 不是有效波特率，请填写 50-4000000",
    validation_baud_range: "{value} 超出支持的波特率范围 50-4000000",
    validation_data_bits_not_number: "{text} 不是有效数据位，请填写 5-8",
    validation_data_bits_range: "{value} 超出支持的数据位范围 5-8",
    validation_address_empty: "请填写地址",
    validation_address_whitespace: "地址不能包含空白字符",
    validation_user_empty: "请填写用户名",
    validation_user_whitespace: "用户名不能包含空白字符",
    validation_name_empty: "请填写名称",
    validation_device_empty: "请填写串口设备路径",
    validation_control_char: "不能包含控制字符",
    validation_group_invalid: "分组路径格式无效",
    hosts_scope_current: "当前结果",
    hosts_scope_all: "全部主机",
    hosts_selection_count: "已选 {count} 台",
    hosts_select_scope: "全选",
    hosts_select_none: "清空",
    hosts_select_start: "选择",
    hosts_filter_clear: "清除筛选",
    hosts_no_match_hint: "改一下搜索词，或去掉一个筛选条件，就能看到更多主机。",
    hosts_group_empty: "还没有分组，点 + 新建",
    hosts_batch_open: "批量任务",
    hosts_batch_open_hint: "确认清单后，在选中的主机上按顺序执行命令",
    batch_title: "批量任务",
    batch_preview_subtitle: "确认之前不会建立任何连接：这里列出的就是运行会用到的目标、命令和限制。",
    batch_targets_section: "目标主机",
    batch_targets_count: "将在 {count} 台主机上执行这些命令",
    batch_scope_label: "目标来源",
    batch_targets_refused: "未纳入",
    batch_targets_more: "还有 {count} 台未列出",
    batch_steps_section: "命令步骤（按顺序执行）",
    batch_step_add: "添加步骤",
    batch_step_remove: "删除步骤",
    batch_step_placeholder: "在每台主机上执行的命令",
    batch_concurrency: "并发主机数",
    batch_concurrency_hint: "1 到 {max}，按连接数计算",
    batch_failure: "主机失败时",
    batch_failure_skip: "跳过该主机，其他继续",
    batch_failure_stop: "停止调度新主机",
    batch_name: "任务名称（可选）",
    batch_name_placeholder: "例行检查",
    batch_confirm: "确认并运行",
    batch_cancel: "取消",
    batch_close: "关闭",
    batch_run_running: "运行中",
    batch_run_paused: "已暂停：不再开始新步骤，正在执行的命令继续",
    batch_run_cancelling: "取消中：已请求停止，远端命令尚未退出",
    batch_run_cancelled: "已取消",
    batch_run_completed: "已完成",
    batch_run_failed: "已结束，存在失败",
    batch_run_interrupted: "应用重启导致中断",
    batch_run_progress: "{done}/{total} 台已结束 · {running} 台运行中 · 并发 {concurrency}",
    batch_run_pause: "暂停",
    batch_run_resume: "继续",
    batch_run_cancel: "取消任务",
    batch_run_retry: "重试失败项",
    batch_run_output: "输出",
    batch_run_output_empty: "该主机暂无输出。",
    batch_run_output_dropped: "较早的 {count} 行未保留在界面上",
    batch_run_timeline: "时间线",
    batch_run_step: "第 {index} 步",
    batch_no_rollback: "重试只执行尚未完成的部分，不会撤销已经执行的内容。",
    batch_retry_note: "重试：只包含未成功的主机与步骤。",
    batch_trust_question: "该主机出示了 {algorithm} {fingerprint}。本次运行是否接受？",
    batch_trust_accept: "接受",
    batch_trust_decline: "拒绝",
    batch_proxy_question: "将要在本机执行 ProxyCommand：{command}",
    batch_count_steps: "{count} 个步骤",
    batch_exit: "退出码 {code}",
    batch_from_step: "从第 {step} 步开始",
    batch_no_targets: "请先选择主机，或选择一个包含主机的范围，再确认。",
    batch_reason_no_credential: "{host} 没有保存的登录信息，请先在主机表单中补上",
    batch_reason_missing_host: "保存的主机已不存在",
    batch_save_failed: "任务定义未能保存",
    batch_log_failed: "运行日志未能写入",
    batch_switch_library: "任务库",
    batch_switch_run: "本次运行",
    batch_switch_history: "历史",
    batch_switch_editor: "编辑中",
    batch_center_open: "批量任务",
    batch_center_open_hint: "编辑任务、查看本窗口的运行，或翻阅历史运行",
    batch_library_title: "已保存的任务",
    batch_library_subtitle: "打开或编辑都不会启动任何任务：只有确认才会开始运行。",
    batch_library_empty_title: "还没有保存的任务",
    batch_library_empty_body: "在主机页选择主机并打开批量编辑器；保存会留下定义，确认后才会运行。",
    batch_library_new: "新建任务",
    batch_library_edit: "编辑",
    batch_library_delete: "删除",
    batch_library_delete_ask: "删除这个任务？它的历史运行仍会保留。",
    batch_library_delete_yes: "删除",
    batch_library_delete_no: "保留",
    batch_library_deleted: "任务已删除",
    batch_library_row_meta: "{hosts} 台主机 · {steps} 个步骤 · 并发 {concurrency}",
    batch_library_more: "还有 {count} 个任务未列出",
    batch_untitled: "未命名任务",
    batch_saved: "已保存，未运行任何命令",
    batch_save_draft: "保存任务",
    batch_step_up: "上移这一步",
    batch_step_down: "下移这一步",
    batch_history_title: "运行历史",
    batch_history_subtitle: "保存在本机的运行摘要。上次退出时未结束的运行会标为中断，且不会重放。",
    batch_history_empty: "还没有运行记录",
    batch_history_filter: "状态",
    batch_history_filter_any: "全部状态",
    batch_history_omitted: "另有 {count} 条较早运行未列出",
    batch_history_damaged: "这条记录无法读取",
    batch_history_open: "详情",
    batch_history_back: "全部运行",
    batch_history_no_host: "选择一台主机查看它的输出与时间线。",
    batch_history_when: "开始于 {time}",
    batch_history_hosts: "{done}/{total} 台主机已结束",
    batch_history_output_unread: "这个主机的输出没有读到。",
    batch_history_output_unread_reason: "这个主机的输出没有读到：{reason}",
    batch_diagnostics_copy: "复制诊断",
    batch_diagnostics_copied: "诊断已复制，其中不含凭据",
    batch_run_background: "转入后台继续运行",
    batch_run_background_hint: "运行会继续；点击顶栏的活动指示可回到本页。",
    batch_indicator_running: "批量 {done}/{total}",
    batch_indicator_failed: "批量：{failed} 台失败",
    batch_run_save_failed: "运行记录未能写入",
    batch_retry_confirm: "重试只执行未成功的主机与步骤，不撤销已执行的内容。",
    batch_step_pending: "等待",
    batch_step_running: "执行中",
    batch_step_succeeded: "成功",
    batch_step_failed: "失败",
    batch_step_skipped: "跳过",
    batch_step_cancelled: "已取消",
    batch_step_already_done: "此前已完成",
    batch_step_interrupted: "已中断",
    batch_host_queued: "排队中",
    batch_host_connecting: "连接中",
    batch_host_running: "执行中",
    batch_host_waiting: "等待你确认",
    batch_host_succeeded: "成功",
    batch_host_failed: "失败",
    batch_host_cancelled: "已取消",
    batch_host_skipped: "已跳过",
    batch_host_interrupted: "已中断",
    field_clear: "清空",
    terminal_jump_bottom: "回到底部（{lines} 行）",
    terminal_jump_bottom_short: "回到底部",
    terminal_select_all: "全选",
    terminal_font_reset: "恢复默认字号（{size}px）",
    host_copy_target: "复制地址",
    home_quick_placeholder: "搜索已保存的主机，或输入 user@host:port 直接连接",
    home_quick_temporary: "连接到 {target}",
    home_quick_temporary_hint: "不保存 · 打开登录",
    home_quick_enter: "回车",
    home_enter_first_hint: "按回车连接这台主机 · ↑↓ 选择其他主机",
    home_recent_strip: "最近",
    home_recent_strip_hint: "连接到 {target} · {when}",
    home_recent_strip_all: "查看全部",
    home_more_ways: "更多开始方式",
    home_more_import_hint: "导入 ~/.ssh/config 中的主机",
    home_more_restore_hint: "从备份文件恢复主机、密钥和设置",
    home_more_quick: "快速连接",
    home_more_quick_hint: "在搜索框输入 user@host 后按回车",
    home_local_button: "本地终端",
    home_serial_section: "串口",
    home_serial_new: "新建串口连接",
    home_favorites_section: "收藏",
    home_card_more: "更多操作",
    home_sort_label: "排序",
    home_sort_saved: "保存顺序",
    home_sort_name: "名称",
    home_sort_recent: "最近连接时间",
    home_filter: "筛选与排序",
    home_sidebar_collapse: "收起侧栏",
    home_sidebar_expand: "展开侧栏",
    home_favorites_empty: "还没有收藏",
    home_favorites_empty_hint: "在主机上点星标，它就会出现在这里。",
    favorite_add: "加入收藏",
    favorite_remove: "取消收藏",
    favorite_add_named: "收藏 {name}",
    favorite_remove_named: "取消收藏 {name}",
    favorite_added: "已加入收藏",
    favorite_removed: "已取消收藏",
    terminal_more: "更多终端工具",
    terminal_grid_size: "终端尺寸",
    session_connecting_card_hint: "连接建立后，终端会在这里打开。",
    banner_reconnecting: "正在重新连接 {target}\u{2026}",
    banner_cancel: "取消",
    banner_dismiss: "关闭（Esc）",
    banner_diagnostic: "诊断信息",
    banner_copy_diagnostic: "复制诊断",
    palette_host_jumps: "{count} 个跳板",
    palette_host_jumps_one: "1 个跳板",
    palette_host_proxy_command: "代理命令",
    hosts_group_empty_body: "这个分组下还没有保存的连接。",
    hosts_delete_selected: "删除所选",
    hosts_delete_selected_title: "删除所选的 {count} 台主机？",
    hosts_delete_selected_body: "这些已保存的主机将从本机移除；当前已打开的会话保持运行。",
    hosts_deleted_many: "已删除 {count} 台主机",
    host_jump_keys: "↑↓ 选择 · Enter 添加 · Esc 关闭",
    pane_zoomed: "已放大",
    focus_mode_enter: "专注视图",
    focus_mode_exit: "退出专注视图",
    pane_divider_reset_hint: "拖动调整比例 · 双击平分窗格",
    desktop_pan_hint: "按住 Shift 滚动可平移画面",
    toast_dismiss: "关闭通知",
    files_go_to_path: "前往路径\u{2026}",
    files_go_to_title: "前往文件夹",
    files_go_to_action: "打开文件夹",
    transfers_clear_finished: "清理已完成",
    fwd_missing_host: "规则 {rule}：主机 {host} 未保存，因此该规则未启动",
    batch_output_copy: "复制输出",
    batch_output_copied: "输出已复制",
    batch_run_in_progress_refusal: "当前窗口已有正在运行的批量任务；请等待结束或返回查看。",
    batch_filter_all: "全部",
    batch_filter_failed: "失败",
    batch_filter_waiting: "待确认",
    batch_filter_running: "运行中",
    batch_filter_done: "已完成",
    snippet_search_placeholder: "搜索快捷命令\u{2026}",
    snippet_search_empty: "没有匹配的快捷命令。",
    snippet_keys_hint: "↑↓ 选择 · Enter 填入 · {run_key} 执行 · Esc 关闭",
    quick_address_placeholder: "example.com 或 192.0.2.10",
    settings_reset_default: "重置",
    sync_unsaved_changes: "有未保存的修改——请先点击保存再同步",
    host_move_to_group: "移动到分组\u{2026}",
    hosts_move_single_title: "将“{name}”移动到分组",
    host_open_files: "打开文件（SFTP）",
    hosts_group_new_host_here: "在此分组新建主机",
    hosts_group_new_subgroup: "新建子分组",
    terminal_jump_bottom_new: "有新输出 \u{00b7} 回到底部（{lines} 行）",
    terminal_open_link: "打开链接",
    terminal_copy_link: "复制链接",
    terminal_clear_scrollback: "清空终端与回滚",
    terminal_snippets_menu: "代码片段\u{2026}",
    snippet_palette_aliases: "代码片段",
    session_global_forwards: "全部端口转发\u{2026}",
};

/// The redesigned-shell strings of the current language.
#[must_use]
pub fn ux() -> &'static UxStrings {
    ux_for(Locale::current())
}

/// The redesigned-shell strings of one language, for wording that has to be produced
/// for a language the caller names rather than for the one on screen.
#[must_use]
pub fn ux_for(locale: Locale) -> &'static UxStrings {
    match locale {
        Locale::English => &ENGLISH_UX,
        Locale::Chinese => &CHINESE_UX,
    }
}

// ---------------------------------------------------------------------------
// Backup export and import (R5, R6, R9)
// ---------------------------------------------------------------------------

/// Declares [`BackupStrings`] and its two tables from one list, so a phrase cannot be
/// added in one language and forgotten in the other.
macro_rules! backup_strings {
    ($($(#[doc = $doc:literal])* $field:ident: $en:literal, $zh:literal;)*) => {
        /// Strings of the backup export and import in the settings' data category.
        pub struct BackupStrings {
            $($(#[doc = $doc])* pub $field: &'static str,)*
        }

        impl BackupStrings {
            /// Every string of the table, in declaration order, for the completeness
            /// test.
            #[must_use]
            #[cfg_attr(not(test), allow(dead_code))]
            pub fn all(&self) -> Vec<&'static str> {
                vec![$(self.$field),*]
            }
        }

        /// The backup strings in English.
        pub static ENGLISH_BACKUP: BackupStrings = BackupStrings { $($field: $en,)* };
        /// The backup strings in Chinese.
        pub static CHINESE_BACKUP: BackupStrings = BackupStrings { $($field: $zh,)* };
    };
}

backup_strings! {
    /// Settings row: export a backup
    settings_export: "Export backup", "导出备份";
    /// Under that row
    settings_export_hint: "Saves hosts, groups, forwards, commands and preferences to one file",
        "把主机、分组、转发、常用命令和偏好保存到一个文件";
    /// Settings row: import a backup
    settings_import: "Import backup", "导入备份";
    /// Under that row
    settings_import_hint: "Previews a backup file, then merges it or replaces this computer's data",
        "先预览备份文件，再合并或覆盖本机数据";
    /// The button of the export row
    export_open: "Export…", "导出…";
    /// The button of the import row
    import_open: "Choose file…", "选择文件…";
    /// Home first-run card: restore from a backup
    home_restore: "Restore from backup", "从备份恢复";
    /// Title of the export dialog
    export_title: "Export backup", "导出备份";
    /// What the export holds
    export_intro: "The file holds your hosts, groups, port forwards, saved commands, trusted server keys and preferences. Private key files are not copied, only their paths.",
        "文件包含主机、分组、端口转发、常用命令、已信任的服务器指纹和偏好设置。私钥文件本身不会被复制，只记录它们的路径。";
    /// The checkbox
    export_include_secrets: "Include saved passwords", "包含已保存的密码";
    /// Under the checkbox, when it is on
    export_secrets_hint: "The passwords are encrypted with a passphrase you choose. Without it nobody can read them, and Shellune cannot recover it.",
        "密码会用你设置的口令加密。没有口令谁都无法读取，Shellune 也无法找回口令。";
    /// Under the checkbox, when it is off
    export_no_secrets: "No password will be written to the file.", "文件中不会写入任何密码。";
    /// Label of the passphrase field
    passphrase: "Passphrase", "口令";
    /// Label of the confirmation field
    passphrase_confirm: "Confirm passphrase", "确认口令";
    /// Export refused: no passphrase
    passphrase_required: "Enter a passphrase to protect the passwords", "请输入用于保护密码的口令";
    /// Export refused: the two differ
    passphrase_mismatch: "The two passphrases are not the same", "两次输入的口令不一致";
    /// The export button
    export_choose: "Choose where to save…", "选择保存位置…";
    /// Export finished
    export_done: "Backup saved", "备份已保存";
    /// Where it went
    export_done_path: "Saved to {path}", "已保存到 {path}";
    /// How many passwords it holds
    export_done_secrets: "{count} saved passwords are in the file, encrypted", "文件中包含 {count} 个已加密的密码";
    /// It holds none
    export_done_no_secrets: "The file holds no password", "文件中不含任何密码";
    /// The passwords that could not be read
    export_unreadable: "The credential store could not read the passwords of these connections, so they are not in the file:",
        "凭据库无法读取以下连接的密码，因此未写入文件：";
    /// The passwords the credential store does not have
    export_missing: "These connections are marked as remembering a password, but the credential store has none for them, so it is not in the file:",
        "以下连接标记为记住密码，但凭据库中没有对应的密码，因此未写入文件：";
    /// Export failed
    export_failed: "The backup could not be saved", "备份未能保存";
    /// Close a finished dialog
    close: "Close", "关闭";
    /// Leave a dialog
    cancel: "Cancel", "取消";
    /// Title of the import dialog
    import_title: "Import backup", "导入备份";
    /// The file was refused
    import_refused: "This file cannot be imported. Nothing on this computer was changed.",
        "无法导入这个文件，本机数据没有任何改动。";
    /// Ask for the passphrase
    import_locked: "This backup holds encrypted passwords. Enter the passphrase it was exported with.",
        "这个备份包含加密的密码，请输入导出时设置的口令。";
    /// Open the passwords
    import_unlock: "Unlock", "解锁";
    /// Go on without them
    import_skip_secrets: "Import without passwords", "不导入密码";
    /// Wrong passphrase
    wrong_passphrase: "The passphrase is wrong, or the file was changed after it was exported.",
        "口令错误，或文件在导出后被改动过。";
    /// Where the file came from
    import_source: "Made on {platform} by Shellune {version}, {date}", "由 {platform} 上的 Shellune {version} 于 {date} 生成";
    /// Count row: connections
    preview_hosts: "Connections", "连接";
    /// Count row: groups
    preview_groups: "Groups", "分组";
    /// Count row: forwards
    preview_forwards: "Port forwards", "端口转发";
    /// Count row: saved commands
    preview_snippets: "Saved commands", "常用命令";
    /// Count row: trusted keys
    preview_known_hosts: "Trusted server keys", "已信任的服务器指纹";
    /// Count row: preferences
    preview_preferences: "Preferences", "偏好设置";
    /// Count row: passwords
    preview_secrets: "Saved passwords", "已保存的密码";
    /// One count row's changes
    preview_change: "{added} new · {updated} changed · {removed} removed", "新增 {added} · 更新 {updated} · 删除 {removed}";
    /// Nothing of this kind changes
    preview_unchanged: "No change", "无变化";
    /// Preferences change
    preview_preferences_changed: "Will change", "将会更改";
    /// Including the keys
    preview_keymap_changed: "Will change, including this computer's shortcuts", "将会更改，包括本机快捷键";
    /// Passwords to save
    preview_secrets_count: "{count} will be saved on this computer", "将在本机保存 {count} 个";
    /// No password in the file
    preview_secrets_none: "None in the file", "文件中没有";
    /// Passwords skipped
    preview_secrets_skipped: "Not imported", "不导入";
    /// Private keys missing here
    preview_missing_keys: "These private key files are not on this computer. The connections are imported; choose the key again before connecting:",
        "以下私钥文件在本机不存在。连接仍会导入，连接前请重新选择私钥：";
    /// The path came from another machine
    preview_key_from: "path from {platform}", "{platform} 上的路径";
    /// The passwords a plan removes, counted
    preview_secrets_forget: "{count} saved on this computer will be removed", "将从本机移除 {count} 个";
    /// The passwords a plan removes, named
    preview_forget_names: "Passwords that will be removed from this computer, because their connection is removed or no longer signs in with one:",
        "以下连接的密码将从本机移除，因为连接被删除或不再使用密码登录：";
    /// One server whose trusted key differs
    preview_trust_line: "{server}: this computer {local}, file {file}", "{server}：本机 {local}，文件 {file}";
    /// Paths taken from another platform
    preview_fallbacks: "The file has no paths of this computer for these connections, so the ones from {platform} are used. Check them before connecting:",
        "文件中没有这些连接在本机的路径，因此沿用 {platform} 上的路径。连接前请确认：";
    /// Trusted keys that differ
    preview_trust_conflicts: "{count} servers have a different trusted key in the file. Merge keeps this computer's, replace uses the file's.",
        "{count} 台服务器在文件中的已信任指纹与本机不同。合并时保留本机的，覆盖时使用文件中的。";
    /// Label of the mode choice
    mode_label: "How to apply", "应用方式";
    /// Merge
    mode_merge: "Merge", "合并";
    /// Replace
    mode_replace: "Replace", "覆盖";
    /// What merge does
    mode_merge_hint: "The file's version of a record wins; records only on this computer are kept.",
        "同一条记录以文件为准，只在本机存在的记录会保留。";
    /// What replace does
    mode_replace_hint: "This computer's connections, groups, forwards, commands and trusted keys are replaced by the file's.",
        "本机的连接、分组、转发、常用命令和已信任指纹会被文件内容整体替换。";
    /// The second question of a replace
    replace_confirm: "Replace deletes everything on this computer that is not in the file. A copy of the current data is kept in the snapshots folder first. Press Confirm replace to go on.",
        "覆盖会删除本机上文件中没有的全部内容，应用前会先在快照文件夹保存一份当前数据。点击“确认覆盖”继续。";
    /// Apply a merge
    apply_merge: "Import", "导入";
    /// Ask to replace
    apply_replace: "Replace…", "覆盖…";
    /// Really replace
    apply_replace_confirm: "Confirm replace", "确认覆盖";
    /// The file changes nothing
    preview_nothing: "This computer already has everything in the file.", "本机已包含文件中的全部内容。";
    /// Import finished
    import_done: "Backup imported", "备份已导入";
    /// The passwords saved by the import
    import_done_secrets: "{count} passwords were saved on this computer.", "已在本机保存 {count} 个密码。";
    /// The passwords the credential store would not remove
    import_not_forgotten: "The credential store would not remove the passwords of these connections. Nothing uses them any more:",
        "凭据库未能移除以下连接的密码，它们已不再被使用：";
    /// What it did
    import_done_body: "The lists and preferences now show the imported data. The previous data was copied to {path}.",
        "列表和偏好已更新为导入后的数据。原数据已复制到 {path}。";
    /// It did nothing
    import_done_nothing: "Nothing needed to change.", "无需任何更改。";
    /// Not a backup
    error_not_backup: "This is not a Shellune backup file.", "这不是 Shellune 备份文件。";
    /// A newer build made it
    error_newer: "A newer Shellune made this backup (format {found}; this version reads up to {supported}). Update Shellune to import it.",
        "这个备份由更新版本的 Shellune 生成（格式 {found}，当前版本最多支持 {supported}），请先更新 Shellune。";
    /// Damaged
    error_corrupt: "The backup file is damaged.", "备份文件已损坏。";
    /// Could not read the file
    error_read: "The file could not be read.", "无法读取这个文件。";
    /// Could not write the file
    error_write: "The file could not be written.", "无法写入这个文件。";
    /// This machine's data could not be read
    error_local: "The data on this computer could not be read.", "无法读取本机数据。";
    /// The data changed after the preview
    error_changed: "The data on this computer changed after the preview. The preview was refreshed; check it and apply again.",
        "预览之后本机数据发生了变化。预览已刷新，请确认后再次应用。";
    /// The snapshot failed
    error_snapshot: "A copy of the current data could not be made, so nothing was changed.",
        "无法先保存当前数据的副本，因此没有做任何更改。";
    /// Applying stopped
    error_apply: "Applying stopped part way.", "应用中途失败。";
    /// And was undone
    error_apply_restored: "The previous data was put back.", "已恢复原来的数据。";
    /// And was not fully undone
    error_apply_not_restored: "The previous data could not be put back completely; a copy is in {path}.",
        "原来的数据未能完全恢复，副本在 {path}。";
    /// Encrypting failed
    error_encrypt: "The passwords could not be encrypted.", "无法加密密码。";
    /// What went wrong, in the storage layer's words
    error_detail: "Details: {reason}", "详情：{reason}";
    /// Unlocking an import with an empty passphrase
    import_passphrase_required: "Enter the passphrase this backup was exported with",
        "请输入导出这个备份时设置的口令";
    /// A host whose private key file is not on this computer (list row chip)
    host_key_missing: "Key file not found on this computer", "本机未找到该私钥文件";
    /// The chip's tooltip, naming the path
    host_key_missing_tip: "{path} does not exist on this computer. Choose the key again before connecting.",
        "本机不存在 {path}，连接前请重新选择私钥。";
    /// Exporting is running in the background
    export_running: "Saving the backup…", "正在保存备份…";
    /// Unlocking or applying is running in the background
    import_running: "Working…", "正在处理…";
    /// Export checkbox: take the whole vault along
    export_include_vault: "Include the vault (keys, passwords, host trust)",
        "包含保险库（密钥、密码、主机信任）";
    /// Under the vault checkbox: why the sync passphrase is asked for
    export_vault_hint: "A backup with the vault's secrets always needs the non-empty sync passphrase, even when WebDAV has a password.",
        "包含保险库秘密的备份始终需要非空同步口令，即使 WebDAV 已设置密码。";
    /// The saved sync passphrase is used, no field is drawn
    export_vault_saved: "The saved sync passphrase seals this backup; it is not retyped.",
        "此备份使用已保存的同步口令密封，无需再次输入。";
    /// Field label when no sync passphrase is saved yet
    export_vault_passphrase: "Sync passphrase", "同步口令";
    /// Confirmation field label
    export_vault_confirm: "Repeat the sync passphrase", "重复输入同步口令";
    /// Refusal: the two fields differ
    export_vault_mismatch: "The two passphrases do not match.", "两次输入的口令不一致。";
    /// Refusal: no passphrase and none saved
    export_vault_required: "Enter the sync passphrase; it also becomes the one your other devices use.",
        "请输入同步口令；它也会成为其他设备使用的口令。";
    /// What the export wrote, with the vault in it
    export_done_vault: "The backup carries the encrypted vault.", "备份已包含加密的保险库。";
    /// The vault could not be opened for the export
    export_vault_locked: "The vault could not be opened on this device, so it could not be included.",
        "无法在此设备上打开保险库，因此无法包含它。";
    /// Import stage: the file carries the encrypted vault and asks for the passphrase
    import_vault_locked: "This backup carries the encrypted vault. Enter the sync passphrase it was sealed with.",
        "这个备份包含加密的保险库，请输入密封时使用的同步口令。";
    /// Preview row: what the vault part holds
    preview_vault: "Vault", "保险库";
    /// Preview line: the vault part's records and trust
    preview_vault_line: "{keys} keys, {passwords} passwords, {trust} trusted identities",
        "{keys} 把密钥、{passwords} 条密码、{trust} 条信任记录";
    /// Preview line: the vault part replaces this device's vault
    preview_vault_replace: "The file's vault replaces this device's vault.",
        "文件中的保险库将替换此设备的保险库。";
    /// Applied: the vault was restored
    import_done_vault: "The vault was restored with all of its records and trust.",
        "保险库已连同全部记录与信任恢复。";
}

/// The backup strings of one language.
#[must_use]
pub fn backup_for(locale: Locale) -> &'static BackupStrings {
    match locale {
        Locale::English => &ENGLISH_BACKUP,
        Locale::Chinese => &CHINESE_BACKUP,
    }
}

// ---------------------------------------------------------------------------
// WebDAV sync (R7, R8, R9, R10)
// ---------------------------------------------------------------------------

/// Declares [`SyncStrings`] and its two tables from one list, so a phrase cannot be
/// added in one language and forgotten in the other.
macro_rules! sync_strings {
    ($($(#[doc = $doc:literal])* $field:ident: $en:literal, $zh:literal;)*) => {
        /// Strings of the sync settings, the conflict dialog and the sync marker.
        pub struct SyncStrings {
            $($(#[doc = $doc])* pub $field: &'static str,)*
        }

        impl SyncStrings {
            /// Every string of the table, in declaration order, for the completeness
            /// test.
            #[must_use]
            #[cfg_attr(not(test), allow(dead_code))]
            pub fn all(&self) -> Vec<&'static str> {
                vec![$(self.$field),*]
            }
        }

        /// The sync strings in English.
        pub static ENGLISH_SYNC: SyncStrings = SyncStrings { $($field: $en,)* };
        /// The sync strings in Chinese.
        pub static CHINESE_SYNC: SyncStrings = SyncStrings { $($field: $zh,)* };
    };
}

sync_strings! {
    /// Settings category and its one page
    settings_title: "Sync", "同步";
    /// Under the page title
    settings_hint: "Keeps hosts, groups, favourites, saved commands, forwards, trusted keys and preferences the same on every computer through one WebDAV folder.",
        "通过一个 WebDAV 文件夹，让每台电脑上的主机、分组、收藏、常用命令、转发、已信任指纹和偏好保持一致。";
    /// Field: the server address
    url: "Server address", "服务器地址";
    /// Under it
    url_hint: "The WebDAV address of your account", "WebDAV 账户地址";
    /// Field: the user name
    username: "User name", "用户名";
    /// Field: the password
    password: "Password", "密码";
    /// Under it
    password_hint: "Kept in the system credential store, never in a file", "保存在系统凭据库中，不写入任何文件";
    /// Placeholder of a secret that is already saved
    secret_saved: "Saved, leave empty to keep it", "已保存，留空则保持不变";
    /// Field: the remote folder
    remote_dir: "Folder", "远端目录";
    /// Under it
    remote_dir_hint: "The folder on the server that holds the data file", "服务器上存放数据文件的文件夹";
    /// Switch: automatic sync
    auto: "Automatic sync", "自动同步";
    /// Under it
    auto_hint: "Pulls when Shellune starts and every 15 minutes, and uploads a few seconds after a change",
        "启动时和每 15 分钟从云端拉取，修改后几秒内自动上传";
    /// A switch that is on
    on: "On", "开";
    /// A switch that is off
    off: "Off", "关";
    /// Switch: sync saved passwords
    secrets: "Sync saved passwords", "同步已保存的密码";
    /// Under it
    secrets_hint: "Off: the cloud holds no password. On: they are encrypted with the sync passphrase, which every computer needs.",
        "关闭时云端不含任何密码；开启后用同步口令加密，每台电脑都需要设置同一口令。";
    /// Field: the sync passphrase
    passphrase: "Sync passphrase", "同步口令";
    /// Under it
    passphrase_hint: "Kept in this computer's credential store", "保存在本机凭据库中";
    /// Saving refused: no passphrase
    passphrase_required: "Enter the sync passphrase to sync passwords", "要同步密码，请输入同步口令";
    /// Refused: no address
    url_required: "Enter the server address first", "请先输入服务器地址";
    /// Refused: nothing saved yet
    not_configured: "Save the server first", "请先保存服务器设置";
    /// Button: save the settings
    save: "Save", "保存";
    /// Saved
    saved: "Sync settings saved", "同步设置已保存";
    /// Not saved
    save_failed: "The sync settings could not be saved", "同步设置未能保存";
    /// Button: test the connection
    test: "Test connection", "测试连接";
    /// While testing
    testing: "Testing…", "正在测试…";
    /// The test passed
    test_ok: "Connected. The folder can be written.", "连接成功，目录可写入。";
    /// The test failed
    test_failed: "Connection test failed:", "测试连接失败：";
    /// Button: upload now
    upload: "Upload now", "立即上传";
    /// Button: pull from the cloud
    pull: "Pull from cloud", "从云端拉取";
    /// Button: open the conflict question again
    resolve: "Resolve conflict…", "处理冲突…";
    /// Label of the status line
    status_label: "Last sync:", "上次同步：";
    /// Never synced
    status_never: "Not synced yet", "尚未同步";
    /// Not set up
    status_not_configured: "Not set up. Nothing is sent until a server is saved.", "未配置。保存服务器之前不会发送任何请求。";
    /// A sync is running
    status_running: "Syncing…", "正在同步…";
    /// One status: nothing to do
    result_up_to_date: "Up to date", "已是最新";
    /// One status: uploaded
    result_uploaded: "Uploaded", "已上传";
    /// One status: pulled
    result_pulled: "Pulled from the cloud", "已从云端拉取";
    /// One status: conflict
    result_conflict: "Conflict, waiting for your choice", "有冲突，等待选择";
    /// One status: failed
    result_failed: "Failed: {reason}", "失败：{reason}";
    /// Under a failure
    retry_note: "Your changes are saved on this computer. Sync tries again by itself in a few minutes, and after the next change.",
        "本地修改已保存。同步会在几分钟内自动重试，下次修改后也会重试。";
    /// Toast: uploaded
    toast_uploaded: "Uploaded to the cloud", "已上传到云端";
    /// Toast: pulled
    toast_pulled: "Synced from the cloud", "已从云端同步";
    /// Toast: both sides changed, one only by connecting; merged without asking
    toast_merged: "Synced; recent connections were merged", "已同步，已自动合并最近连接";
    /// Toast: nothing to do
    toast_up_to_date: "Already up to date", "已是最新";
    /// Toast: failed
    toast_failed: "Sync failed", "同步失败";
    /// After a pull: trusted keys replaced
    pull_trust: "{count} servers had a different trusted key; the cloud's is used now.",
        "{count} 台服务器的已信任指纹与云端不同，现已使用云端的。";
    /// After a pull: passwords in the cloud not used
    pull_secrets_ignored: "The cloud holds encrypted passwords; turn on syncing passwords to use them.",
        "云端含有加密的密码；开启“同步已保存的密码”后才会使用。";
    /// After a pull: passwords that could not be removed
    pull_not_forgotten: "{count} passwords no longer used could not be removed from the credential store.",
        "{count} 个已不再使用的密码未能从凭据库移除。";
    /// Top bar marker: failed
    marker_failed: "Sync failed", "同步失败";
    /// Top bar marker: conflict
    marker_conflict: "Sync conflict", "同步冲突";
    /// The marker's tooltip
    marker_hint: "Open the sync settings", "打开同步设置";
    /// Conflict dialog title
    conflict_title: "Sync conflict", "同步冲突";
    /// Both changed
    conflict_intro: "This computer and the cloud both changed since the last sync. Nothing was overwritten. Choose which one to keep.",
        "自上次同步以来，本机和云端都有修改。两边都还没有被覆盖，请选择保留哪一边。";
    /// First sync, both hold data
    conflict_first: "The cloud already holds Shellune data that differs from this computer's. Nothing was overwritten. Choose which one to keep.",
        "云端已有与本机不同的 Shellune 数据。两边都还没有被覆盖，请选择保留哪一边。";
    /// Where the cloud data came from
    conflict_source: "Cloud data written on {platform} by Shellune {version}, {date}",
        "云端数据由 {platform} 上的 Shellune {version} 于 {date} 写入";
    /// What "use cloud" does
    conflict_remote_hint: "Use cloud data: this computer's data is replaced; a copy is kept in its snapshots folder first.",
        "以云端为准：本机数据会被替换，替换前先在本机快照文件夹保存一份。";
    /// What "use local" does
    conflict_local_hint: "Use this computer's data: the cloud file is replaced; a copy of it is kept in this computer's snapshots folder first.",
        "以本机为准：云端文件会被替换，替换前先把云端文件保存到本机快照文件夹。";
    /// What cancel does
    conflict_cancel_hint: "Cancel: nothing changes, and the conflict stays until you choose.",
        "取消：两边都不变，冲突会保留到你做出选择。";
    /// Button: use the cloud
    use_remote: "Use cloud data", "以云端为准";
    /// Button: use this computer
    use_local: "Use this computer's data", "以本机为准";
    /// Button: cancel
    cancel: "Cancel", "取消";
    /// Refused sign-in
    error_auth: "Authentication failed: the server refused the user name or password.",
        "认证失败：服务器拒绝了用户名或密码。";
    /// No answer
    error_unreachable: "The server could not be reached.", "无法连接到服务器。";
    /// Folder not writable
    error_not_writable: "The folder \"{path}\" cannot be written.", "目录“{path}”不可写入。";
    /// Bad address
    error_address: "The server address cannot be used.", "服务器地址无效。";
    /// Another HTTP failure
    error_server: "The server answered with an error.", "服务器返回了错误。";
    /// Not set up
    error_not_configured: "Sync is not set up.", "尚未配置同步。";
    /// sync.json problem
    error_settings: "The sync settings could not be read or written.", "无法读取或写入同步设置。";
    /// Keyring problem
    error_credentials: "The credential store could not be read.", "无法读取系统凭据库。";
    /// No passphrase saved
    error_passphrase_missing: "Syncing passwords is on, but no sync passphrase is saved on this computer.",
        "已开启同步密码，但本机没有保存同步口令。";
    /// Another passphrase in the cloud
    error_remote_passphrase: "The passwords in the cloud are encrypted with another sync passphrase.",
        "云端的密码使用了不同的同步口令加密。";
    /// Cloud file unusable
    error_remote_document: "The data file in the cloud cannot be used.", "云端的数据文件无法使用。";
    /// Nothing to pull
    error_nothing_in_cloud: "There is no Shellune data in the cloud yet.", "云端还没有 Shellune 数据。";
    /// Local data problem
    error_local: "The data on this computer could not be read or written.", "无法读取或写入本机数据。";
    /// Kept changing
    error_busy: "The data kept changing during the sync; it will be tried again.",
        "同步过程中数据一直在变化，稍后会重试。";
    /// Detail suffix
    error_detail: "Details: {reason}", "详情：{reason}";
    /// A failure kept from an earlier run whose reason this build does not know
    error_last_failed: "The last sync failed.", "上次同步失败。";
    /// Toast: edits made while data arrived could not all be kept
    held_dropped: "Some changes could not be combined with the synced data",
        "部分修改无法与同步来的数据合并";
    /// Its detail
    held_dropped_detail: "The data from the sync was kept for: {list}. Make those changes again.",
        "以下内容保留了同步来的数据：{list}。请重新进行这些修改。";
    /// Toast: edits made while data arrived could not be written
    held_failed: "Changes made during the sync could not be saved", "同步期间的修改未能保存";
    /// Toast: a close waits until the edits held during a sync are written
    exit_waits: "Saving your changes before closing", "正在保存修改，随后关闭";
    /// Its detail
    exit_waits_detail: "Shellune closes as soon as the sync in progress ends and your changes are on disk.",
        "当前同步结束、修改写入本机后，Shellune 会自动关闭。";
    /// Toast: the waiting close was called off because some edits were not kept
    exit_stopped: "Shellune stayed open so you can redo those changes",
        "Shellune 未关闭，以便你重新进行这些修改";
    /// Data document: hosts
    doc_hosts: "hosts", "主机";
    /// Data document: groups
    doc_groups: "groups", "分组";
    /// Data document: forwards
    doc_forwards: "port forwards", "端口转发";
    /// Data document: saved commands
    doc_snippets: "saved commands", "常用命令";
    /// Data document: trusted keys
    doc_trust: "trusted keys", "已信任指纹";
    /// Data document: preferences
    doc_preferences: "preferences", "偏好";
    /// Vault conflict dialog title
    vault_conflict_title: "Vault conflicts", "保险库冲突";
    /// Vault conflict dialog intro
    vault_conflict_intro: "Both sides changed these vault records differently. Nothing has been applied; each record is decided on its own, and the vault stays as it is until every one is answered.",
        "两端对以下保险库记录做了不同修改。目前没有应用任何内容；每条记录单独决定，全部答复之前保险库保持不变。";
    /// One record conflict, both sides changed it
    vault_record_label: "{name} \u{b7} changed {count} times", "{name} · 修改了 {count} 次";
    /// One record conflict, one side deleted it
    vault_record_deleted_label: "{name} \u{b7} deleted on this device", "{name} · 此设备已删除";
    /// One trust conflict
    vault_trust_label: "{host}:{port} \u{b7} trusted identity", "{host}:{port} · 信任身份";
    /// Caption of a side: which device wrote it
    vault_side_device: "{device} \u{b7} {count} changes", "{device} · {count} 次修改";
    /// Caption of a side: deleted
    vault_side_deleted: "deleted", "已删除";
    /// Caption of a side: a fingerprint, as the trust conflict shows it
    vault_side_fingerprint: "{algorithm} \u{b7} {fingerprint}", "{algorithm}，指纹 {fingerprint}";
    /// Button: keep this device's version
    vault_keep_local: "Keep this device's", "保留此设备";
    /// Button: keep the cloud's version
    vault_keep_remote: "Keep the cloud's", "保留云端";
    /// Button: apply all the choices made
    vault_apply: "Apply choices", "应用选择";
    /// The whole-document question waits behind the vault one
    vault_then_document: "The data outside the vault is decided after these records.",
        "这些记录处理完后，再决定保险库之外的数据。";
    /// Automatic sync waits while the vault is locked
    deferred_locked: "Automatic sync waits until the vault is unlocked", "自动同步等待保险库解锁";
    /// A pull applied the cloud's vault
    pull_vault: "The cloud's vault was applied (version {version}).",
        "已应用云端保险库（版本 {version}）。";
}

/// The sync strings of one language.
#[must_use]
pub fn sync_for(locale: Locale) -> &'static SyncStrings {
    match locale {
        Locale::English => &ENGLISH_SYNC,
        Locale::Chinese => &CHINESE_SYNC,
    }
}

// ---------------------------------------------------------------------------
// Updates (R2, R3)
// ---------------------------------------------------------------------------

/// Declares [`UpdateStrings`] and its two tables from one list, so a phrase cannot be
/// added in one language and forgotten in the other.
macro_rules! update_strings {
    ($($(#[doc = $doc:literal])* $field:ident: $en:literal, $zh:literal;)*) => {
        /// Strings of the about-and-updates page, the update chip and the question
        /// asked before an install.
        pub struct UpdateStrings {
            $($(#[doc = $doc])* pub $field: &'static str,)*
        }

        impl UpdateStrings {
            /// Every string of the table, in declaration order, for the completeness
            /// test.
            #[must_use]
            #[cfg_attr(not(test), allow(dead_code))]
            pub fn all(&self) -> Vec<&'static str> {
                vec![$(self.$field),*]
            }
        }

        /// The update strings in English.
        pub static ENGLISH_UPDATE: UpdateStrings = UpdateStrings { $($field: $en,)* };
        /// The update strings in Chinese.
        pub static CHINESE_UPDATE: UpdateStrings = UpdateStrings { $($field: $zh,)* };
    };
}

update_strings! {
    /// Settings category: about and updates
    settings_title: "About & updates", "关于与更新";
    /// The update page in it
    page_title: "Updates", "更新";
    /// Under the page title
    page_hint: "Shellune looks for new releases on GitHub. Every update file is checked against Shellune's signing key before anything is installed.",
        "Shellune 从 GitHub 发布页查找新版本。任何更新文件都要先通过 Shellune 签名密钥的校验才会安装。";
    /// Row: the running version
    current_version: "Current version", "当前版本";
    /// Switch: check automatically
    auto: "Check for updates automatically", "自动检查更新";
    /// Under it
    auto_hint: "Once, about 20 seconds after Shellune starts. A choice of this computer only; it is not synced.",
        "Shellune 启动约 20 秒后检查一次。仅对本机生效，不会同步。";
    /// A switch that is on
    on: "On", "开";
    /// A switch that is off
    off: "Off", "关";
    /// Button: check now
    check: "Check for updates", "检查更新";
    /// While checking
    checking: "Checking…", "正在检查…";
    /// Not checked yet
    status_idle: "Not checked since Shellune started.", "本次启动后尚未检查。";
    /// The newest version
    status_latest: "Shellune {version} is the newest version. Checked {date}.",
        "已是最新版本（{version}），检查时间 {date}。";
    /// Toast after a manual check: nothing new
    toast_latest: "You have the newest version", "已是最新版本";
    /// A newer release
    available_title: "Shellune {version} is available", "发现新版本 Shellune {version}";
    /// When it was published
    available_date: "Published {date}", "发布于 {date}";
    /// Heading of the notes
    notes_label: "What's new", "更新说明";
    /// No notes
    no_notes: "This release has no notes here; the release page has the details.",
        "此版本没有附带更新说明，详情见发布页。";
    /// Button: download, then install
    download_install: "Download and install", "下载并安装";
    /// While downloading
    downloading: "Downloading…", "正在下载…";
    /// Progress with a known size
    progress_of: "{percent}% · {done} of {total}", "{percent}% · 已下载 {done}，共 {total}";
    /// Progress without one
    progress_bytes: "{done} received", "已接收 {done}";
    /// Downloaded and verified
    ready: "Downloaded and verified. Ready to install.", "已下载并通过签名校验，等待安装。";
    /// Button: install
    install: "Install now", "立即安装";
    /// While installing
    installing: "Installing…", "正在安装…";
    /// The Windows installer route
    installer_note: "The installer runs silently: Shellune closes, and the new version starts by itself.",
        "安装程序会静默运行：Shellune 将关闭，新版本会自动启动。";
    /// Installed in place
    restart_ready: "Shellune {version} is installed. Restart to use it.", "Shellune {version} 已安装，重启后生效。";
    /// Button: restart
    restart: "Restart now", "立即重启";
    /// Button: the release page
    release_page: "Open release page", "打开发布页";
    /// Button: try again
    retry: "Try again", "重试";
    /// Failure heading: the check
    failed_check: "Checking for updates failed", "检查更新失败";
    /// Failure heading: the download
    failed_download: "The update could not be downloaded", "更新下载失败";
    /// Failure heading: the install
    failed_install: "The update could not be installed", "更新安装失败";
    /// Under every failure
    unchanged: "Shellune {version} is still installed; nothing of it was changed.",
        "当前仍是 Shellune {version}，未做任何改动。";
    /// Only the release page: this packaging
    manual_platform: "This installation is updated from the release page: download the new package there.",
        "这种安装方式需要从发布页下载新版本手动更新。";
    /// Only the release page: a debug build
    manual_debug: "A development build does not install updates itself; download the release from its page.",
        "开发构建不会自动安装更新，请从发布页下载。";
    /// Only the release page: the folder cannot be written
    manual_no_permission: "Shellune's folder cannot be written without administrator rights, so the update has to be installed from the release page.",
        "Shellune 所在文件夹没有写入权限（需要管理员权限），请从发布页手动安装更新。";
    /// Only the release page: no file for this computer
    no_asset: "This release has no update file for {platform}, so it cannot be installed from here.",
        "此版本没有适用于本机（{platform}）的更新文件，无法在此安装。";
    /// Only the release page: the file is listed without a signature
    signature_missing_listed: "The update file for {platform} is published without a signature, so it is not downloaded.",
        "适用于本机（{platform}）的更新文件缺少签名，因此不会下载。";
    /// Platform: Windows installer
    platform_windows_installer: "Windows, installed", "Windows 安装版";
    /// Platform: Windows portable
    platform_windows_portable: "Windows, portable", "Windows 便携版";
    /// Platform: macOS
    platform_macos: "macOS, Apple silicon", "macOS（Apple 芯片）";
    /// Platform: Linux
    platform_linux: "Linux, deb package", "Linux（deb 包）";
    /// Error: settings
    error_config: "The update settings of this build cannot be used.", "此版本的更新设置无效。";
    /// Error: the check went unanswered
    error_check_timeout: "The update server did not answer in time.", "更新服务器响应超时。";
    /// Error: the download went quiet
    error_download_timeout: "The download timed out: the server stopped sending data. The partial file was deleted.",
        "下载超时：服务器长时间没有发送数据。未完成的文件已删除。";
    /// Error: unreachable
    error_unreachable: "The update server could not be reached.", "无法连接到更新服务器。";
    /// Error: HTTP status
    error_http: "The update server answered HTTP {status}.", "更新服务器返回 HTTP {status}。";
    /// Error: manifest
    error_manifest: "The update information could not be read.", "无法读取更新信息。";
    /// Error: no file for this computer
    error_no_asset: "This release has no update file for this installation.", "此版本没有适用于本机的更新文件。";
    /// Error: no signature
    error_signature_missing: "The update file has no signature, so it was not downloaded.",
        "更新文件缺少签名，已拒绝下载。";
    /// Error: signature does not verify
    error_signature_invalid: "Signature check failed: the downloaded file is not the one Shellune signed. It was deleted.",
        "签名校验失败：下载的文件不是 Shellune 签名发布的文件，已删除。";
    /// Error: signed for another version
    error_version_mismatch: "Signature check failed: the file was signed for {signed}, not for version {expected}. It was deleted.",
        "签名校验失败：文件的签名对应 {signed}，而不是版本 {expected}，已删除。";
    /// Error: interrupted
    error_interrupted: "The download was interrupted. The partial file was deleted.", "下载中断，未完成的文件已删除。";
    /// Error: files
    error_io: "An update file could not be read or written.", "无法读写更新文件。";
    /// Error: install
    error_install: "The update could not be put in place; the previous files were restored.",
        "无法替换为新版本，已恢复原来的文件。";
    /// Detail suffix
    error_detail: "Details: {reason}", "详情：{reason}";
    /// Top bar chip
    chip: "New version v{version}", "新版本 v{version}";
    /// Its tooltip
    chip_hint: "Open About & updates", "打开关于与更新";
    /// Question before installing
    confirm_title: "Install the update now?", "现在安装更新？";
    /// Question before restarting
    confirm_restart_title: "Restart Shellune now?", "现在重启 Shellune？";
    /// What the question says
    confirm_intro: "These are still running and will be closed. Your tabs and layout are saved first and come back after the restart.",
        "以下会话仍在运行，将会被关闭。标签与布局会先保存，重启后恢复。";
    /// Count: terminals
    confirm_terminals: "{count} terminal sessions", "{count} 个终端会话";
    /// Count: transfers
    confirm_transfers: "{count} file transfers", "{count} 个文件传输";
    /// Count: remote desktops
    confirm_desktops: "{count} remote desktops", "{count} 个远程桌面";
    /// Count: serial sessions
    confirm_serials: "{count} serial sessions", "{count} 个串口会话";
    /// Button: install anyway
    confirm_install: "Install and restart", "安装并重启";
    /// Button: restart anyway
    confirm_restart: "Restart", "重启";
    /// Button: not now
    later: "Later", "稍后";
    /// Toast: the new version did not start
    restart_failed: "The new version could not be started", "无法启动新版本";
    /// Toast: waiting for held saves before installing
    install_waits: "Saving your changes before the update", "正在保存修改，随后更新";
    /// Its detail
    install_waits_detail: "The update goes on as soon as the sync in progress ends and your changes are on disk.",
        "当前同步结束、修改写入本机后，更新会继续进行。";
    /// Toast: the update was called off because some edits were not kept
    install_stopped: "The update was not installed so you can redo those changes",
        "未进行更新，以便你重新进行这些修改";
}

/// The update strings of one language.
#[must_use]
pub fn update_for(locale: Locale) -> &'static UpdateStrings {
    match locale {
        Locale::English => &ENGLISH_UPDATE,
        Locale::Chinese => &CHINESE_UPDATE,
    }
}

// ---------------------------------------------------------------------------
// The window's start (R3)
// ---------------------------------------------------------------------------

/// Declares [`StartupStrings`] and its two tables from one list, so a phrase cannot be
/// added in one language and forgotten in the other.
macro_rules! startup_strings {
    ($($(#[doc = $doc:literal])* $field:ident: $en:literal, $zh:literal;)*) => {
        /// Strings of the start page: the stage under the progress bar and the stop a
        /// failed stage makes.
        pub struct StartupStrings {
            $($(#[doc = $doc])* pub $field: &'static str,)*
        }

        impl StartupStrings {
            /// Every string of the table, in declaration order, for the completeness
            /// test.
            #[must_use]
            #[cfg_attr(not(test), allow(dead_code))]
            pub fn all(&self) -> Vec<&'static str> {
                vec![$(self.$field),*]
            }
        }

        /// The start strings in English.
        pub static ENGLISH_STARTUP: StartupStrings = StartupStrings { $($field: $en,)* };
        /// The start strings in Chinese.
        pub static CHINESE_STARTUP: StartupStrings = StartupStrings { $($field: $zh,)* };
    };
}

startup_strings! {
    /// Stage 1 under the bar, while it runs
    stage_settings: "Reading settings and theme", "正在读取设置与主题";
    /// Stage 2
    stage_hosts: "Reading hosts and known servers", "正在读取主机簿与已知主机";
    /// Stage 3
    stage_workspace: "Reading forwards, groups and workspace", "正在读取转发、分组与工作区";
    /// Stage 4
    stage_interface: "Preparing the interface", "正在准备界面";
    /// Every stage is done; the page comes next
    stage_done: "Ready", "准备就绪";
    /// Stop title: stage 1 failed
    failed_settings: "The settings could not be read", "无法读取设置";
    /// Stop title: stage 2 failed
    failed_hosts: "The host book could not be read", "无法读取主机簿";
    /// Stop title: stage 3 failed
    failed_workspace: "Forwards, groups or the workspace could not be read",
        "无法读取转发、分组或工作区";
    /// Stop title: stage 4 failed
    failed_interface: "The interface could not be prepared", "无法准备界面";
    /// Reason of a settings file that is not a settings document
    settings_unreadable: "{file} is not a settings file this version can read. The default settings are in use until one is changed.",
        "{file} 不是此版本能读取的设置文件。在修改任一设置之前，将使用默认设置。";
    /// Reason when a data file could not be read from disk
    storage_read_failed: "{file} could not be read.",
        "无法读取 {file}。";
    /// Reason when a data file is damaged or invalid
    storage_corrupt: "{file} is damaged or not a valid data file.",
        "{file} 已损坏或格式无效。";
    /// Reason when a data file uses a newer format version
    storage_unsupported_version: "{file} uses format {found}, which is newer than this version supports ({supported}).",
        "{file} 使用了更新的格式版本（{found}），当前版本最高支持 {supported}。";
    /// Fallback reason when a data file could not be loaded
    storage_failed: "{file} could not be loaded.",
        "无法加载 {file}。";
    /// Appends technical error detail after a localized summary
    error_detail: "{summary} ({detail})",
        "{summary}（详情：{detail}）";
    /// Visual cue above the stop buttons indicating the details area can scroll
    scroll_hint: "Scroll or use ↑/↓ / PageUp / PageDown for full details",
        "滚动或按 ↑/↓ / PageUp / PageDown 查看完整详情";
    /// Under the reason: what the two buttons do
    failed_hint: "Continue opens Shellune with what could be read. The files are in the data folder.",
        "选择“继续”将以已读取的内容打开 Shellune。相关文件都在配置文件夹中。";
    /// Button: go on with what could be read
    continue_start: "Continue", "继续";
    /// Button: open the folder the files are in
    open_folder: "Open data folder", "打开配置文件夹";
}

/// The start strings of one language.
#[must_use]
pub fn startup_for(locale: Locale) -> &'static StartupStrings {
    match locale {
        Locale::English => &ENGLISH_STARTUP,
        Locale::Chinese => &CHINESE_STARTUP,
    }
}

// ---------------------------------------------------------------------------
// Keys and passwords of the vault (R1, ADR-7)
// ---------------------------------------------------------------------------

/// Declares [`SecurityStrings`] and its two tables from one list, so a phrase cannot
/// be added in one language and forgotten in the other.
macro_rules! security_strings {
    ($($(#[doc = $doc:literal])* $field:ident: $en:literal, $zh:literal;)*) => {
        /// Strings of the settings' "keys and passwords" page and its dialogs.
        pub struct SecurityStrings {
            $($(#[doc = $doc])* pub $field: &'static str,)*
        }

        impl SecurityStrings {
            /// Every string of the table, in declaration order, for the completeness
            /// test.
            #[must_use]
            #[cfg_attr(not(test), allow(dead_code))]
            pub fn all(&self) -> Vec<&'static str> {
                vec![$(self.$field),*]
            }
        }

        /// The security strings in English.
        pub static ENGLISH_SECURITY: SecurityStrings = SecurityStrings { $($field: $en,)* };
        /// The security strings in Chinese.
        pub static CHINESE_SECURITY: SecurityStrings = SecurityStrings { $($field: $zh,)* };
    };
}

security_strings! {
    /// Settings category: keys, passwords and (later) trust and biometrics
    settings_category: "Security", "安全";
    /// The one page of the category this build draws
    settings_item: "Keys & passwords", "密钥与密码";
    /// Under the page's name
    settings_hint: "The SSH keys and connection passwords this app keeps in its own vault",
        "管理应用自身保险库保存的 SSH 密钥与连接密码";
    /// Sign-in method chooser entry
    method_label: "Application key", "应用密钥";
    /// Under the chooser entry
    method_hint: "Signs in with a private key kept in this app's vault; no key file is read",
        "使用应用保险库中的私钥登录，不再读取密钥文件";
    /// The connection form, when the method is chosen and the vault holds no key yet
    method_none: "The vault holds no application key yet. Import or generate one in Settings.",
        "保险库中还没有应用密钥，请先在设置中导入或生成。";
    /// The connection form, when the key the host names cannot be handed over
    managed_missing: "The vault cannot hand this key over. Open the keys page and check the vault.",
        "保险库无法提供这把密钥，请到密钥页检查保险库。";
    /// A desktop connection start, when the shared password it names cannot be
    /// handed over - the vault is locked, or the record is gone
    credential_unavailable: "The vault cannot hand this password over. Open the keys page and check the vault.",
        "保险库无法提供这个密码，请到密钥页检查保险库。";
    /// The connection form, naming the key that will sign
    managed_chosen: "Signing in with: {name}", "登录密钥：{name}";
    /// Search field placeholder of the page
    search_placeholder: "Search by name, user, algorithm, fingerprint or host",
        "按名称、用户名、算法、指纹或主机搜索";
    /// Section heading
    keys_title: "SSH keys", "SSH 密钥";
    /// Section heading
    credentials_title: "Passwords", "密码";
    /// No key is in the vault
    empty_keys: "No key is in the vault yet.", "保险库中还没有密钥。";
    /// No password is in the vault
    empty_credentials: "No password is in the vault yet.", "保险库中还没有密码。";
    /// Nothing matches the search
    no_match: "No record matches the search.", "没有匹配的记录。";
    /// Chip of a key row
    kind_key: "SSH key", "SSH 密钥";
    /// Chip of a password row
    kind_credential: "Password", "密码";
    /// How many connections reference a record
    hosts_count: "{count} connections", "{count} 个连接";
    /// A record no connection references
    not_referenced: "Not used by a connection", "未被连接使用";
    /// Created caption
    created: "Created {date}", "创建于 {date}";
    /// Changed caption
    changed: "Changed {date}", "修改于 {date}";
    /// Button: import a private key
    import_key: "Import key…", "导入密钥…";
    /// Button: generate a key
    generate_key: "Generate key…", "生成密钥…";
    /// Button: new shared password
    new_password: "New password…", "新建密码…";
    /// Dialog button
    cancel: "Cancel", "取消";
    /// Dialog button
    save: "Save", "保存";
    /// Dialog button
    close: "Close", "关闭";
    /// Detail button
    edit: "Edit", "编辑";
    /// The copy control of one value a person copies elsewhere
    copy_value: "Copy", "复制";
    /// Detail button of a referenced record, per host
    unbind: "Unbind", "解绑";
    /// Detail button: a copy of a shared password with its own identity
    duplicate: "Independent copy", "独立副本";
    /// Detail button
    delete: "Delete", "删除";
    /// Detail button
    rotate: "Rotate…", "轮换…";
    /// Password reveal toggle
    show: "Show", "显示";
    /// Password hide toggle
    hide: "Hide", "隐藏";
    /// Detail label
    label_algorithm: "Algorithm", "算法";
    /// Detail label
    label_fingerprint: "Fingerprint", "指纹";
    /// Detail label
    label_public_key: "Public key (authorized_keys)", "公钥（authorized_keys）";
    /// Detail label
    label_comment: "Comment", "注释";
    /// Detail label
    label_user: "User", "用户名";
    /// Detail label
    label_password: "Password", "密码";
    /// Detail heading: the connections that reference this record
    referenced_by: "Used by", "使用的连接";
    /// Under the lists: the rule about secrets
    secrets_hidden: "Secrets stay hidden by default; show or copy one at a time.",
        "秘密默认不展示，可按需单独显示或复制。";
    /// Page line while a vault task runs on the background executor
    busy: "Working…", "处理中…";
    /// Notice prefix when the vault could not be opened or written
    vault_failed: "The vault could not be opened", "无法打开保险库";
    /// Notice prefix when a write was refused
    vault_refused: "The vault refused the change", "保险库拒绝了本次修改";
    /// Import dialog title
    import_title: "Import a private key", "导入私钥";
    /// Import dialog intro
    import_intro: "The key is re-protected by this app's vault. The original file is not changed, moved or read again.",
        "密钥导入后由应用保险库重新保护。原文件不会被修改、移动或再次读取。";
    /// Import field
    import_pem: "OpenSSH private key text", "OpenSSH 私钥内容";
    /// Import field
    import_passphrase: "Key passphrase, when it has one", "密钥口令（如有）";
    /// Import field
    import_name: "Name", "名称";
    /// Import refusal: nothing typed
    import_missing: "Paste the private key text and give it a name.", "请粘贴私钥内容并填写名称。";
    /// Import refusal: same public key again, naming the record that holds it
    import_duplicate: "A key with this public key is already in the vault: {name}",
        "保险库中已有相同公钥的密钥：{name}";
    /// Import refusal: RSA below the floor this build accepts
    import_rsa_weak: "An RSA key of {bits} bits is too weak to import; 2048 is the least this build accepts.",
        "{bits} 位的 RSA 密钥过弱，不予导入；本应用最低接受 2048 位。";
    /// Import notice: allowed, but weaker than what this build generates
    import_rsa_small: "The RSA key is {bits} bits, below the 3072 this build generates; replacing it is worth planning.",
        "这把 RSA 密钥为 {bits} 位，低于本应用生成密钥的 3072 位，建议计划更换。";
    /// Generate dialog title
    generate_title: "Generate a key", "生成密钥";
    /// Generate field
    generate_name: "Name", "名称";
    /// Generate field
    generate_comment: "Comment (optional)", "注释（可选）";
    /// Refusal: no name
    name_required: "A name is required.", "名称不能为空。";
    /// Edit key dialog title
    edit_key_title: "Edit key details", "编辑密钥信息";
    /// Edit key dialog note
    edit_key_note: "Renaming changes neither the key material nor its fingerprint.",
        "重命名不会改变密钥本体和指纹。";
    /// Export dialog title
    export_title: "Export private key", "导出私钥";
    /// Export dialog intro
    export_intro: "The export passphrase protects this one file and is never saved. It is not the WebDAV password and not a sync passphrase.",
        "导出口令只保护这一个文件，不会被保存。它不是 WebDAV 密码，也不是同步口令。";
    /// Export field
    export_passphrase: "Export passphrase", "导出口令";
    /// Export field
    export_confirm: "Confirm passphrase", "确认口令";
    /// Export checkbox: no passphrase at all
    export_plain: "Export without a passphrase (plain text)", "不加口令导出（明文）";
    /// The risk sentence a plain export shows
    export_plain_risk: "A plain export holds the private key unencrypted. Anyone who can read the file can use the key.",
        "明文导出的私钥不含加密。任何能读到该文件的人都可以使用这把密钥。";
    /// The second, explicit confirmation of a plain export
    export_plain_confirm: "I understand the risk and want a plain file", "我理解风险，确认导出明文";
    /// Export refusal: passphrase missing or mismatching
    export_passphrase_required: "Type the export passphrase the same way twice.", "请输入两次一致的导出口令。";
    /// Export refusal: the two differ
    export_mismatch: "The two passphrases are not the same", "两次输入的口令不一致";
    /// Export primary button
    export_choose: "Choose where to save…", "选择保存位置…";
    /// Export finished
    export_done: "Private key exported", "私钥已导出";
    /// Where the export went
    export_done_path: "Saved to {path}", "已保存到 {path}";
    /// Export failed
    export_failed: "The private key could not be exported", "私钥导出失败";
    /// Rotate dialog title
    rotate_title: "Rotate a key", "轮换密钥";
    /// Rotate dialog intro
    rotate_intro: "The chosen connections move to a newly generated key. Deploy the new public key on the servers yourself; this app does not change any server.",
        "所选连接将迁移到新生成的密钥。新公钥需要你自行部署到服务器，此操作不会改动任何服务器。";
    /// Rotate field
    rotate_new_name: "New key name", "新密钥名称";
    /// Rotate heading
    rotate_hosts: "Connections to move", "要迁移的连接";
    /// Rotate checkbox
    rotate_retire: "Delete the old key once nothing uses it", "旧密钥不再被使用后删除";
    /// Under the unchecked retire box
    rotate_keep_hint: "The old key is kept until you delete it yourself.", "旧密钥会保留，直到你自行删除。";
    /// Rotate refusal
    rotate_none: "Choose at least one connection to move.", "请至少选择一个要迁移的连接。";
    /// Rotate finished
    rotate_done: "{count} connections moved to the new key", "{count} 个连接已迁移到新密钥";
    /// Rotate result heading
    rotate_public: "New public key - deploy it on the servers", "新公钥——请部署到服务器";
    /// Delete key dialog title
    delete_key_title: "Delete a key", "删除密钥";
    /// Delete key dialog: the connections that will lose their sign-in
    delete_affected: "These connections sign in with it and will have to choose a way again:",
        "以下连接正在使用它，删除后需要重新选择登录方式：";
    /// Delete password dialog title
    delete_credential_title: "Delete a password", "删除密码";
    /// Delete finished (a key)
    delete_key_done: "Key deleted", "密钥已删除";
    /// Delete finished (a password)
    delete_credential_done: "Password deleted", "密码已删除";
    /// A record no connection references
    delete_none_affected: "No connection uses it.", "没有连接使用它。";
    /// New password dialog title
    new_credential_title: "New password", "新建密码";
    /// Field
    credential_name: "Name", "名称";
    /// Field
    credential_user: "User (optional)", "用户名（可选）";
    /// Field
    credential_password: "Password", "密码";
    /// Button: fill the field with a generated value
    generate_password: "Generate", "生成";
    /// Charset checkbox
    charset_letters: "Letters a-z A-Z", "字母 a-z A-Z";
    /// Charset checkbox
    charset_digits: "Digits 0-9", "数字 0-9";
    /// Charset checkbox
    charset_symbols: "Symbols", "符号";
    /// Length field label
    password_length: "Length", "长度";
    /// Refusal: neither typed nor generated
    password_missing: "Type a password or generate one.", "请输入或生成密码。";
    /// Edit password dialog title
    edit_credential_title: "Edit password", "编辑密码";
    /// Edit password field: empty keeps the current value
    edit_credential_password: "New password (empty keeps the current one)", "新密码（留空保持不变）";
    /// A saved password edit reached every referencing connection
    credential_edited: "Saved. Every connection that shares this password now uses the new value.",
        "已保存。共享此密码的所有连接即使用新值。";
    /// A password record was created
    credential_created: "Password saved", "密码已保存";
    /// An independent copy was created
    credential_duplicated: "Independent copy created; it changes on its own from here on.",
        "已创建独立副本，此后它单独变化。";
    /// One host was unbound
    unbind_done: "Unbound from {host}", "已解除与 {host} 的绑定";
    /// A vault job overlapped edits the user made meanwhile
    drift_kept: "Your edits to {names} were kept; the vault's reference change was applied on top of them.",
        "对 {names} 的修改已保留，保险库的引用变更在其之上生效。";
    /// A key record was saved (create, generate or metadata edit)
    key_saved: "Key saved", "密钥已保存";
    /// Hint under the host editor's key picker
    host_key_pick: "The key this host signs in with", "此主机使用的登录密钥";
    /// Hint under the host editor's shared-password picker
    host_credential_pick: "Use a password from the vault instead of typing one",
        "使用保险库中的共享密码，不再手动输入";
    /// Host editor: stop using a vault password
    host_credential_clear: "Type a password instead", "改为手动输入密码";
    /// Settings item: the host trust page
    trust_settings_item: "Host trust", "主机信任";
    /// Under the trust page's name
    trust_settings_hint: "The server fingerprints you have accepted, searchable and removable",
        "已接受的服务器指纹，可搜索、可删除";
    /// Search field placeholder of the trust page
    trust_search_placeholder: "Search by host, port, algorithm or fingerprint",
        "按主机、端口、算法或指纹搜索";
    /// Section heading of the trust list
    trust_title: "Accepted server identities", "已接受的服务器身份";
    /// No trust record exists
    trust_empty: "No server identity has been accepted yet.", "还没有接受过服务器身份。";
    /// Nothing matches the search
    trust_no_match: "No accepted identity matches the search.", "没有匹配的信任记录。";
    /// Row caption: where the record came from
    trust_from_vault: "vault", "保险库";
    /// Row caption: where the record came from
    trust_from_this_mac: "this device only", "仅此设备";
    /// Detail label of a trust row
    trust_accepted: "Accepted {date}", "接受于 {date}";
    /// Delete dialog title of one trust record
    trust_delete_title: "Remove trusted identity", "移除信任身份";
    /// Delete dialog body
    trust_delete_body: "The next connection to {server} asks about this server again, as an unknown host. No password is needed for this.",
        "下次连接 {server} 时将按未知主机重新询问。此操作不需要任何口令。";
    /// Delete dialog button
    trust_delete_action: "Remove", "移除";
    /// Delete finished
    trust_deleted: "Trusted identity removed", "已移除信任身份";
    /// Delete finished, naming the server
    trust_deleted_note: "Trusted identity removed for {server}", "已移除 {server} 的信任身份";
    /// Settings item: the lock and biometrics page
    lock_settings_item: "Lock & biometrics", "锁定与生物识别";
    /// Under the lock page's name
    lock_settings_hint: "When this device asks you to verify again before opening the vault",
        "此设备在何时要求重新验证后才能使用保险库";
    /// Lock status: the vault is open on this device
    lock_state_open: "Vault unlocked on this device", "保险库在此设备上已解锁";
    /// Lock status: the vault is locked
    lock_state_locked: "Vault locked", "保险库已锁定";
    /// Lock status while a verification is running
    lock_state_verifying: "Asking the system to verify you\u{2026}", "正在请求系统验证\u{2026}";
    /// Button: lock now
    lock_now: "Lock now", "立即锁定";
    /// Button: unlock now
    lock_unlock: "Unlock", "解锁";
    /// Switch label: unlock this device's vault with biometrics
    lock_biometric: "Unlock with biometrics", "使用生物识别解锁";
    /// Under the biometrics switch
    lock_biometric_hint: "Touch ID, Windows Hello or the system's own prompt, when this device offers one",
        "此设备支持时使用 Touch ID、Windows Hello 或系统提示";
    /// The biometric switch is on; the system says what it can do
    lock_capability_biometrics: "This device verifies with its biometric sensor.",
        "此设备使用生物识别传感器进行验证。";
    /// The system can re-verify the user, but not with biometrics
    lock_capability_system: "This device re-verifies with the system user (a password or PIN); it is not biometric strength.",
        "此设备以系统用户方式重新验证（密码或 PIN），并非生物识别强度。";
    /// The system offers nothing this app can call
    lock_capability_none: "This device offers no system verification this app can call.",
        "此设备没有可供应用调用的系统验证。";
    /// Label of the policy segmented control
    lock_policy_label: "Ask again", "重新验证时机";
    /// Policy choice
    lock_policy_unlock: "To unlock", "解锁时";
    /// Policy choice
    lock_policy_every: "Every sensitive operation", "每次敏感操作";
    /// Under the policy control, the unlock-vault choice
    lock_policy_unlock_hint: "One verification opens the vault until it locks again.",
        "验证一次即解锁保险库，直到再次锁定。";
    /// Under the policy control, the every-operation choice
    lock_policy_every_hint: "Showing or copying a password, signing with a managed key and exporting each verify again.",
        "显示或复制密码、使用受管密钥认证、导出等每次都重新验证。";
    /// Label of the idle lock field
    lock_idle_label: "Lock after idle", "空闲后锁定";
    /// Under the idle lock field
    lock_idle_hint: "Minutes without a sensitive use, 1 to 1440; a sleep counts.",
        "敏感操作空闲的分钟数（1 到 1440），休眠同样计入。";
    /// Refusal of the idle minutes field
    lock_idle_refusal: "Enter minutes between 1 and 1440.", "请输入 1 到 1440 之间的分钟数。";
    /// The saved lock choices could not be applied
    lock_settings_problem: "The saved lock choices could not be read; the defaults are in use.",
        "已保存的锁定设置无法读取，正在使用默认值。";
    /// Verification was cancelled; nothing opened
    lock_cancelled: "Verification was cancelled; the vault stays locked.", "验证已取消，保险库保持锁定。";
    /// Verification did not match; nothing opened
    lock_failed: "Verification did not match; the vault stays locked.", "验证未通过，保险库保持锁定。";
    /// The system could not ask right now
    lock_busy: "The system could not ask right now; try again in a moment.",
        "系统暂时无法弹出验证，请稍后再试。";
    /// The enrolled biometrics changed, so this device's protection was reset
    lock_enrollment_changed: "The enrolled biometrics changed, so biometric unlocking was turned off on this device. Verify with the current WebDAV password or sync passphrase, then turn it on again.",
        "已登记的生物识别发生变化，此设备的生物识别解锁已停用。请用当前 WebDAV 密码或同步口令重新解锁后再开启。";
    /// A sensitive operation was blocked while the vault is locked
    lock_blocked: "The vault is locked. Unlock it in Settings to use this.",
        "保险库已锁定，请在设置中解锁后再使用。";
    /// Re-enabling biometrics after it was reset needs a fresh verification
    lock_reenable_note: "Turning biometrics back on asks the system to verify you once.",
        "重新开启生物识别会请求系统验证一次。";
    /// Risk dialog: enabling on a machine with no system verification
    biometric_risk_title: "No system verification on this device",
        "此设备没有可用的系统验证";
    /// Risk dialog body: what the unlocked-session shape is and is not
    biometric_risk_body: "This device offers no verification this app can call, so the protection is the unlocked session itself: anyone using this device can open the vault. It is not biometric strength. The vault is never locked automatically in this shape, and the choice can be turned off at any time.",
        "此设备没有可供应用调用的系统验证，因此保护仅来自已解锁的用户会话：使用此设备的任何人都可以打开保险库。这不等同于生物识别强度。此形态下保险库不会自动锁定，且可随时关闭。";
    /// Risk dialog confirm button
    biometric_risk_confirm: "Turn on anyway", "仍然开启";
    /// Home sidebar & Vault workbench title
    vault_nav_title: "Vault", "密码库";
    /// Home sidebar trailing badge when vault is locked
    vault_locked_badge: "Locked", "已锁定";
    /// Command palette: open the vault workbench
    palette_open_vault: "Open vault", "打开密码库";
    /// Command palette detail for opening the vault
    palette_open_vault_hint: "SSH keys, shared passwords and connection bindings",
        "管理 SSH 密钥、共享密码与连接绑定";
    /// Command palette: create a shared password
    palette_new_password: "New shared password…", "新建共享密码…";
    /// Command palette: generate an SSH key
    palette_generate_key: "Generate SSH key…", "生成 SSH 密钥…";
    /// Command palette: import an SSH private key
    palette_import_key: "Import SSH key…", "导入 SSH 密钥…";
    /// Command palette: open host trust settings
    palette_open_trust: "Open host trust", "打开主机信任";
    /// Command palette: lock the vault immediately
    palette_lock_now: "Lock vault now", "立即锁定密码库";
    /// Command palette: unlock the vault
    palette_unlock_vault: "Unlock vault", "解锁密码库";
    /// Top bar tooltip when device lock is engaged and vault is unlocked
    topbar_lock_tip: "Vault unlocked — click to lock now", "密码库已解锁——点击立即锁定";
    /// Top bar tooltip when vault is locked
    topbar_unlock_tip: "Vault locked — click to unlock", "密码库已锁定——点击解锁";
    /// Filter segment: all records
    filter_all: "All", "全部";
    /// Filter segment: SSH keys only
    filter_keys: "SSH keys", "SSH 密钥";
    /// Filter segment: shared passwords only
    filter_credentials: "Shared passwords", "共享密码";
    /// Filter segment: records with no bound connection
    filter_unused: "Unused", "未绑定连接";
    /// Empty state when filtering for unused records
    empty_unused: "Every record in the vault is bound to at least one connection.",
        "密码库中的记录均已绑定到连接。";
    /// Empty-selection guide in the wide master-detail vault workbench
    detail_select_hint: "Select a key or password on the left to inspect its details, copy values or manage bound connections.",
        "在左侧选择密钥或共享密码，以查看详情、复制内容或管理绑定的连接。";
    /// Tooltip for copying a key's public key
    copy_public_key: "Copy public key", "复制公钥";
    /// Tooltip for copying a key's fingerprint
    copy_fingerprint: "Copy fingerprint", "复制指纹";
    /// Tooltip for copying a credential's username
    copy_username: "Copy username", "复制用户名";
    /// Tooltip for copying a credential's password
    copy_password: "Copy password", "复制密码";
    /// Toast title when a password is copied to the clipboard
    password_copied: "Password copied", "密码已复制";
    /// Button in import private key dialog to choose a local file
    import_browse_file: "Browse file…", "浏览文件…";
    /// Error prefix when a chosen private key file could not be read
    import_file_failed: "Could not read key file", "无法读取密钥文件";
    /// Disabled tooltip on Rotate when no connection uses the key
    rotate_disabled_unused: "No connection uses this key yet, so there is nothing to rotate.",
        "尚无连接绑定此密钥，无需轮换。";
    /// Host trust page banner when the vault is locked
    trust_locked_hint: "The vault is locked. Server identities stored in the vault are hidden until you unlock it.",
        "密码库已锁定。保存在密码库中的服务器身份在解锁前不会显示。";
    /// Connection dialog locked state for application keys
    host_vault_locked_key: "The vault is locked. Unlock it to choose or add an application key.",
        "密码库已锁定，解锁后可选择或添加应用密钥。";
    /// Connection dialog locked state note when an application key is already bound
    host_vault_locked_bound_key: "This host already references a key in the locked vault; saving keeps that binding.",
        "此主机已绑定密码库中的密钥，直接保存将保留当前绑定。";
    /// Connection dialog locked state for shared passwords
    host_vault_locked_credential: "The vault is locked. Unlock it to pick or create a shared password.",
        "密码库已锁定，解锁后可选择或新建共享密码。";
    /// Connection dialog locked state when a shared password is already bound
    host_vault_locked_bound_credential: "This connection is bound to a shared password in the locked vault.",
        "此连接已绑定密码库中的共享密码（当前密码库已锁定）。";
    /// Warning chip when a bound vault key is missing
    vault_key_missing_chip: "Vault key missing", "密码库密钥缺失";
    /// Warning tooltip/message when a bound vault key is missing
    vault_key_missing_tip: "The application key bound to this host is no longer in the vault. Pick or create another key.",
        "此主机绑定的应用密钥已不在密码库中，请重新选择或创建密钥。";
    /// Warning chip when a bound shared password is missing
    vault_credential_missing_chip: "Shared password missing", "共享密码缺失";
    /// Warning tooltip/message when a bound shared password is missing
    vault_credential_missing_tip: "The shared password bound to this connection is no longer in the vault. Pick another or type a password.",
        "此连接绑定的共享密码已不在密码库中，请重新选择或手动输入密码。";
}

/// The security strings of one language.
#[must_use]
pub fn security_for(locale: Locale) -> &'static SecurityStrings {
    match locale {
        Locale::English => &ENGLISH_SECURITY,
        Locale::Chinese => &CHINESE_SECURITY,
    }
}

/// Why a shortcut command cannot be saved, in a language.
///
/// The domain crate words its refusals in English so they stay language-neutral; the
/// dialog says them in the language of the window, with the limit filled in.
#[must_use]
pub fn snippet_problem_text(problem: &SnippetProblem, locale: Locale) -> String {
    let ux = ux_for(locale);
    match problem {
        SnippetProblem::EmptyName => ux.snippet_problem_name_empty.to_owned(),
        SnippetProblem::NameTooLong { max } => {
            fill(ux.snippet_problem_name_long, &[("max", &max.to_string())])
        }
        SnippetProblem::EmptyBody => ux.snippet_problem_body_empty.to_owned(),
        SnippetProblem::BodyTooLong { max } => {
            fill(ux.snippet_problem_body_long, &[("max", &max.to_string())])
        }
        SnippetProblem::Unknown { .. } => ux.snippet_problem_unknown.to_owned(),
    }
}

/// The name of one tab colour in a language.
///
/// The colour is there to tell tabs apart, so it is also written out: a tab never
/// depends on being seen in colour to be identified.
#[must_use]
pub fn tab_accent_label(accent: TabAccent, locale: Locale) -> &'static str {
    let strings = locale.strings();
    match accent {
        TabAccent::Iris => strings.tab_accent_iris,
        TabAccent::Violet => strings.tab_accent_violet,
        TabAccent::Teal => strings.tab_accent_teal,
        TabAccent::Lime => strings.tab_accent_lime,
        TabAccent::Orchid => strings.tab_accent_orchid,
        TabAccent::Clay => strings.tab_accent_clay,
    }
}

/// How long ago something happened, said the way a list row says it.
///
/// A list of servers is scanned for "which one did I use last", which an absolute UTC
/// stamp answers only after arithmetic. The exact stamp stays one hover away; this is
/// the glanceable part. A moment in the future (a clock that moved back) reads as
/// "just now" instead of a negative age.
#[must_use]
pub fn relative_time(then_unix: u64, now_unix: u64, locale: Locale) -> String {
    let seconds = now_unix.saturating_sub(then_unix);
    let minutes = seconds / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    match locale {
        Locale::English => match () {
            () if minutes < 1 => "just now".to_owned(),
            () if hours < 1 => format!("{minutes} min ago"),
            () if days < 1 => format!("{hours} h ago"),
            () if days == 1 => "yesterday".to_owned(),
            () if days < 30 => format!("{days} days ago"),
            () if days < 365 => format!("{} mo ago", days / 30),
            () => format!("{} y ago", days / 365),
        },
        Locale::Chinese => match () {
            () if minutes < 1 => "刚刚".to_owned(),
            () if hours < 1 => format!("{minutes} 分钟前"),
            () if days < 1 => format!("{hours} 小时前"),
            () if days == 1 => "昨天".to_owned(),
            () if days < 30 => format!("{days} 天前"),
            () if days < 365 => format!("{} 个月前", days / 30),
            () => format!("{} 年前", days / 365),
        },
    }
}

/// Fills `{name}` placeholders.
#[must_use]
pub fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    let mut text = template.to_owned();
    for (name, value) in pairs {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod ux_tests {
    use super::*;

    #[test]
    fn both_ux_tables_are_complete_and_translated() {
        let english = ENGLISH_UX.all();
        let chinese = CHINESE_UX.all();
        assert_eq!(english.len(), chinese.len());
        for (en, zh) in english.iter().zip(chinese.iter()) {
            assert!(!en.trim().is_empty() && !zh.trim().is_empty());
            assert_ne!(en, zh, "{en} is not translated");
        }
    }

    #[test]
    fn relative_time_reads_at_a_glance_in_both_languages() {
        let now = 1_700_000_000;
        assert_eq!(relative_time(now, now, Locale::English), "just now");
        assert_eq!(relative_time(now + 50, now, Locale::Chinese), "刚刚");
        assert_eq!(
            relative_time(now - 5 * 60, now, Locale::English),
            "5 min ago"
        );
        assert_eq!(
            relative_time(now - 3 * 3600, now, Locale::Chinese),
            "3 小时前"
        );
        assert_eq!(relative_time(now - 30 * 3600, now, Locale::Chinese), "昨天");
        assert_eq!(
            relative_time(now - 9 * 86_400, now, Locale::English),
            "9 days ago"
        );
        assert_eq!(
            relative_time(now - 70 * 86_400, now, Locale::Chinese),
            "2 个月前"
        );
        assert_eq!(
            relative_time(now - 800 * 86_400, now, Locale::English),
            "2 y ago"
        );
    }

    #[test]
    fn both_backup_tables_are_complete_and_translated() {
        let english = ENGLISH_BACKUP.all();
        let chinese = CHINESE_BACKUP.all();
        assert_eq!(english.len(), chinese.len());
        for (en, zh) in english.iter().zip(chinese.iter()) {
            assert!(!en.trim().is_empty() && !zh.trim().is_empty());
            assert_ne!(en, zh, "{en} is not translated");
            // A placeholder in one language is filled in the other as well.
            for name in ["{path}", "{count}", "{platform}", "{version}", "{date}"] {
                assert_eq!(en.contains(name), zh.contains(name), "{en} / {zh}: {name}");
            }
        }
    }

    #[test]
    fn both_sync_tables_are_complete_and_translated() {
        let english = ENGLISH_SYNC.all();
        let chinese = CHINESE_SYNC.all();
        assert_eq!(english.len(), chinese.len());
        for (en, zh) in english.iter().zip(chinese.iter()) {
            assert!(!en.trim().is_empty() && !zh.trim().is_empty());
            assert_ne!(en, zh, "{en} is not translated");
            for name in [
                "{path}",
                "{count}",
                "{platform}",
                "{version}",
                "{date}",
                "{reason}",
            ] {
                assert_eq!(en.contains(name), zh.contains(name), "{en} / {zh}: {name}");
            }
        }
    }

    #[test]
    fn both_security_tables_are_complete_and_translated() {
        let english = ENGLISH_SECURITY.all();
        let chinese = CHINESE_SECURITY.all();
        assert_eq!(english.len(), chinese.len());
        for (en, zh) in english.iter().zip(chinese.iter()) {
            assert!(!en.trim().is_empty() && !zh.trim().is_empty());
            assert_ne!(en, zh, "{en} is not translated");
            for name in [
                "{name}", "{count}", "{path}", "{host}", "{bits}", "{date}", "{names}",
            ] {
                assert_eq!(en.contains(name), zh.contains(name), "{en} / {zh}: {name}");
            }
        }
    }

    #[test]
    fn both_startup_tables_are_complete_and_translated() {
        let english = ENGLISH_STARTUP.all();
        let chinese = CHINESE_STARTUP.all();
        assert_eq!(english.len(), chinese.len());
        for (en, zh) in english.iter().zip(chinese.iter()) {
            assert!(!en.trim().is_empty() && !zh.trim().is_empty());
            assert_ne!(en, zh, "{en} is not translated");
            for name in ["{file}", "{found}", "{supported}", "{summary}", "{detail}"] {
                assert_eq!(en.contains(name), zh.contains(name), "{en} / {zh}: {name}");
            }
        }
    }

    #[test]
    fn both_update_tables_are_complete_and_translated() {
        let english = ENGLISH_UPDATE.all();
        let chinese = CHINESE_UPDATE.all();
        assert_eq!(english.len(), chinese.len());
        for (en, zh) in english.iter().zip(chinese.iter()) {
            assert!(!en.trim().is_empty() && !zh.trim().is_empty());
            assert_ne!(en, zh, "{en} is not translated");
            for name in [
                "{version}",
                "{date}",
                "{platform}",
                "{count}",
                "{reason}",
                "{status}",
                "{percent}",
                "{done}",
                "{total}",
                "{expected}",
                "{signed}",
            ] {
                assert_eq!(en.contains(name), zh.contains(name), "{en} / {zh}: {name}");
            }
        }
    }

    #[test]
    fn fill_replaces_every_placeholder() {
        assert_eq!(fill("{a} to {b}", &[("a", "x"), ("b", "y")]), "x to y");
    }

    #[test]
    fn the_scp_hint_gives_each_direction_its_own_example() {
        // Upload types a remote *directory* and Download types the remote *file*; one
        // shared "~/report.txt" example would send the upload direction to a directory
        // that is really a file, and the task would fail on a correctly written path.
        assert!(
            ENGLISH
                .files_sftp_scp_hint
                .contains("remote directory (for example ~)")
        );
        assert!(
            ENGLISH
                .files_sftp_scp_hint
                .contains("remote file (for example ~/report.txt)")
        );
        assert!(CHINESE.files_sftp_scp_hint.contains("远端目录（例如 ~）"));
        assert!(
            CHINESE
                .files_sftp_scp_hint
                .contains("远端文件（例如 ~/报告.txt）")
        );
    }
}
