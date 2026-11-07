//! Host management state: the editor draft, the three list views and the messages.
//!
//! This module is render-independent on purpose. The validation, the search, the
//! grouping and the recency ordering are all decided here, in plain Rust, so they
//! can be tested without a window; `crate::app::view` only draws what this module
//! reports.
//!
//! Two rules shape the state:
//!
//! * a draft is text until it is saved, so an invalid port is refused by the domain
//!   constructor and the editor keeps what the user typed;
//! * a message says what really happened. "Saved" never implies a connection was
//!   opened, and a failure keeps the reason the storage layer produced.

use shellune_core::{AuthMethodKind, HostConfig, HostId, Port};
use shellune_storage::{
    DEFAULT_CONFIG_TEXT, Group, HostBook, HostEdit, ImportPreview, RecentHost, host_matches,
};

use crate::app::copy::Strings;

/// Which subset of the saved hosts the list shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HostView {
    /// Every saved host, in saved order.
    #[default]
    All,
    /// The same hosts, grouped by group label.
    Groups,
    /// The hosts that were really reached, newest first.
    Recent,
}

impl HostView {
    /// Every view, in the order the switch shows them.
    pub const ALL: [Self; 3] = [Self::All, Self::Groups, Self::Recent];

    /// Label of the view switch entry.
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::All => strings.hosts_view_all,
            Self::Groups => strings.hosts_view_groups,
            Self::Recent => strings.hosts_view_recent,
        }
    }

    /// Debug selector of the view switch entry, used by the layout tests.
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::All => "hosts.view.all",
            Self::Groups => "hosts.view.groups",
            Self::Recent => "hosts.view.recent",
        }
    }
}

/// One field of the host form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostField {
    /// The display name.
    Name,
    /// The address the next connection dials.
    Address,
    /// The TCP port.
    Port,
    /// The login user.
    User,
    /// The optional group label.
    Group,
    /// The optional private key path a connection may use.
    ///
    /// The editor only ever holds a path: Shellune never reads the key file, so there
    /// is nothing here that could hold a key or its passphrase.
    IdentityFile,
    /// The password or key passphrase, typed to be remembered.
    ///
    /// The draft holds it only while the form is open; it is handed to the system
    /// credential store on save and never reaches the host record.
    Secret,
    /// The command typed into the remote shell after connecting.
    StartupCommand,
    /// The ProxyCommand template. Blank means none, and nothing is run from the form.
    ProxyCommand,
}

impl HostField {
    /// Every field, in form order.
    pub const ALL: [Self; 9] = [
        Self::Name,
        Self::Address,
        Self::Port,
        Self::User,
        Self::Group,
        Self::IdentityFile,
        Self::Secret,
        Self::StartupCommand,
        Self::ProxyCommand,
    ];

    /// Label shown in front of the field.
    pub fn label(self, strings: &Strings) -> &'static str {
        match self {
            Self::Name => strings.host_field_name,
            Self::Address => strings.host_field_address,
            Self::Port => strings.host_field_port,
            Self::User => strings.host_field_user,
            Self::Group => strings.host_field_group,
            Self::IdentityFile => strings.host_field_identity,
            Self::Secret => strings.auth_password,
            Self::StartupCommand => strings.host_startup_label,
            Self::ProxyCommand => strings.host_proxy_label,
        }
    }

    /// Debug selector of the field, used by the tests.
    pub fn debug_selector(self) -> &'static str {
        match self {
            Self::Name => "hosts.field.name",
            Self::Address => "hosts.field.address",
            Self::Port => "hosts.field.port",
            Self::User => "hosts.field.user",
            Self::Group => "hosts.field.group",
            Self::IdentityFile => "hosts.field.identity",
            Self::Secret => "hosts.field.secret",
            Self::StartupCommand => "hosts.field.startup",
            Self::ProxyCommand => "hosts.field.proxy",
        }
    }
}

/// The text of one host form.
///
/// The values stay text until the form is saved: a port of "0" has to be visible in
/// the field that produced it, so the editor keeps the text and asks the domain
/// whether it is usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDraft {
    editing: Option<HostId>,
    name: String,
    address: String,
    port: String,
    user: String,
    group: String,
    identity: String,
    /// How the host signs in.
    auth: AuthMethodKind,
    /// Whether the password or passphrase is kept in the system credential store.
    remember: bool,
    /// Whether the store already holds one for this host, so an empty field keeps it.
    has_saved_secret: bool,
    /// The secret typed in this form; empty means "keep what is saved".
    secret: String,
    startup: String,
    /// Saved hosts to dial before this one, in order.
    jumps: Vec<HostId>,
    /// ProxyCommand template. Blank clears it and nothing is executed.
    proxy: String,
}

impl HostDraft {
    /// An empty form for a new host.
    #[must_use]
    pub fn create() -> Self {
        Self {
            editing: None,
            name: String::new(),
            address: String::new(),
            port: Port::DEFAULT.to_string(),
            user: String::new(),
            group: String::new(),
            identity: String::new(),
            auth: AuthMethodKind::Password,
            remember: true,
            has_saved_secret: false,
            secret: String::new(),
            startup: String::new(),
            jumps: Vec::new(),
            proxy: String::new(),
        }
    }

    /// A form filled with a saved host.
    #[must_use]
    pub fn edit(host: &HostConfig) -> Self {
        Self {
            editing: Some(host.id()),
            name: host.name().to_owned(),
            address: host.address().to_owned(),
            port: host.port().to_string(),
            user: host.user().to_owned(),
            group: host.group().unwrap_or_default().to_owned(),
            identity: host.identity_file().unwrap_or_default().to_owned(),
            // A host saved before the choice existed signs in with its key when it has
            // one, and with a password otherwise, which is what its form offered.
            auth: host.auth().unwrap_or(if host.identity_file().is_some() {
                AuthMethodKind::KeyFile
            } else {
                AuthMethodKind::Password
            }),
            remember: host.remembers_secret() || host.auth().is_none(),
            has_saved_secret: host.remembers_secret(),
            secret: String::new(),
            startup: host.startup_command().unwrap_or_default().to_owned(),
            jumps: host.jumps().to_vec(),
            proxy: host.proxy_command().unwrap_or_default().to_owned(),
        }
    }

    /// How the host signs in.
    #[must_use]
    pub fn auth(&self) -> AuthMethodKind {
        self.auth
    }

    /// Chooses how the host signs in, returning whether it changed.
    pub fn set_auth(&mut self, auth: AuthMethodKind) -> bool {
        let changed = self.auth != auth;
        self.auth = auth;
        changed
    }

    /// Whether the secret of this method is to be remembered.
    #[must_use]
    pub fn remember(&self) -> bool {
        self.remember
    }

    pub fn set_remember(&mut self, remember: bool) {
        self.remember = remember;
    }

    /// Whether the system store already holds this host's secret.
    #[must_use]
    pub fn has_saved_secret(&self) -> bool {
        self.has_saved_secret
    }

    /// Whether the chosen method reads a password or a passphrase.
    #[must_use]
    pub fn needs_secret(&self) -> bool {
        self.auth.needs_password() || self.auth.needs_passphrase()
    }

    /// Whether the form will keep a secret in the system store when saved.
    ///
    /// Only when there is one to keep: a secret typed now, or one already saved. A
    /// host saved with the box ticked and nothing typed signs in by asking, which is
    /// what "save it now, type the password when connecting" needs.
    #[must_use]
    pub fn keeps_secret(&self) -> bool {
        self.needs_secret() && self.remember && (!self.secret.is_empty() || self.has_saved_secret)
    }

    /// The secret typed in this form, when one was typed.
    #[must_use]
    pub fn typed_secret(&self) -> Option<&str> {
        (!self.secret.is_empty()).then_some(self.secret.as_str())
    }

    /// Forgets the typed secret, once it has been handed to the store.
    pub fn clear_secret(&mut self) {
        self.secret.clear();
    }

    /// The host this form changes, when it is not a new one.
    #[must_use]
    pub fn editing(&self) -> Option<HostId> {
        self.editing
    }

    /// The current text of one field.
    #[must_use]
    pub fn text(&self, field: HostField) -> &str {
        match field {
            HostField::Name => &self.name,
            HostField::Address => &self.address,
            HostField::Port => &self.port,
            HostField::User => &self.user,
            HostField::Group => &self.group,
            HostField::IdentityFile => &self.identity,
            HostField::Secret => &self.secret,
            HostField::StartupCommand => &self.startup,
            HostField::ProxyCommand => &self.proxy,
        }
    }

    /// Replaces the text of one field, returning whether it changed.
    pub fn set_text(&mut self, field: HostField, text: impl Into<String>) -> bool {
        let slot = match field {
            HostField::Name => &mut self.name,
            HostField::Address => &mut self.address,
            HostField::Port => &mut self.port,
            HostField::User => &mut self.user,
            HostField::Group => &mut self.group,
            HostField::IdentityFile => &mut self.identity,
            HostField::Secret => &mut self.secret,
            HostField::StartupCommand => &mut self.startup,
            HostField::ProxyCommand => &mut self.proxy,
        };
        let text = text.into();
        if *slot == text {
            return false;
        }
        *slot = text;
        true
    }

    /// The validated change this form describes.
    ///
    /// # Errors
    ///
    /// Returns the reason the domain rejected the values: an empty name, address or
    /// user, an address with whitespace, or a port outside `1..=65535`. The text is
    /// not changed, so the user can correct the field that was refused.
    pub fn build(&self, existing: Option<&HostConfig>) -> Result<HostEdit, String> {
        let port = parse_port(&self.port)?;
        match existing {
            Some(saved) => {
                let mut host = saved.clone();
                host.set_name(self.name.clone())
                    .map_err(|error| error.to_string())?;
                host.set_address(self.address.clone())
                    .map_err(|error| error.to_string())?;
                host.set_port(u32::from(port.get()))
                    .map_err(|error| error.to_string())?;
                host.set_user(self.user.clone())
                    .map_err(|error| error.to_string())?;
                host.set_group(Some(self.group.clone()));
                // A blank field clears the path instead of storing an empty string, and
                // the value is only ever the path to the key.
                host.set_identity_file(self.identity_for_method());
                self.apply_sign_in(&mut host);
                self.apply_route(&mut host);
                Ok(HostEdit::replace(host))
            }
            None => {
                let mut host = HostConfig::new(
                    self.name.clone(),
                    self.address.clone(),
                    port.get(),
                    self.user.clone(),
                )
                .map_err(|error| error.to_string())?;
                host.set_group(Some(self.group.clone()));
                host.set_identity_file(self.identity_for_method());
                self.apply_sign_in(&mut host);
                self.apply_route(&mut host);
                Ok(HostEdit::create(host))
            }
        }
    }

    /// The jump hosts, in dial order.
    #[must_use]
    pub fn jumps(&self) -> &[HostId] {
        &self.jumps
    }

    /// Adds a saved host to the end of the jump chain.
    ///
    /// The host being edited is not added, and a hop already in the list is not repeated.
    pub fn add_jump(&mut self, id: HostId) -> bool {
        if self.editing == Some(id) || self.jumps.contains(&id) {
            return false;
        }
        self.jumps.push(id);
        true
    }

    /// Removes the hop at `index`.
    pub fn remove_jump(&mut self, index: usize) {
        if index < self.jumps.len() {
            self.jumps.remove(index);
        }
    }

    /// Moves the hop at `index` one place up or down.
    pub fn move_jump(&mut self, index: usize, up: bool) {
        let Some(next) = (if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1)
        }) else {
            return;
        };
        if next < self.jumps.len() {
            self.jumps.swap(index, next);
        }
    }

    /// Whether the form currently asks for both a jump chain and a ProxyCommand.
    #[must_use]
    pub fn proxy_conflicts(&self) -> bool {
        !self.jumps.is_empty() && !self.proxy.trim().is_empty()
    }

    /// Writes the jump chain and the ProxyCommand. A blank command clears it.
    fn apply_route(&self, host: &mut HostConfig) {
        host.set_jumps(self.jumps.iter().copied());
        host.set_proxy_command(Some(self.proxy.clone()));
    }

    /// The key path to store: kept only for a method that reads a key file.
    fn identity_for_method(&self) -> Option<String> {
        self.auth.needs_key_path().then(|| self.identity.clone())
    }

    /// Writes the sign-in choice and the startup command into a record.
    fn apply_sign_in(&self, host: &mut HostConfig) {
        host.set_auth(Some(self.auth));
        host.set_remember_secret(self.keeps_secret());
        host.set_startup_command(Some(self.startup.clone()));
    }
}

/// Parses the port field.
///
/// An empty field means the SSH default; anything else goes through the domain's
/// range check, which is the only place that decides whether a port is usable.
fn parse_port(text: &str) -> Result<Port, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Port::DEFAULT);
    }
    let raw: u32 = text
        .parse()
        .map_err(|_| format!("{text} is not a port number; use 1-65535"))?;
    Port::new(raw).map_err(|error| error.to_string())
}

/// What the last action produced, in the words of the layer that produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostsMessage {
    text: String,
    failed: bool,
}

impl HostsMessage {
    /// The text to show.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the action failed, which decides the tone.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed
    }
}

/// Everything the hosts destination holds apart from the records themselves.
#[derive(Debug, Default)]
pub struct HostsPanel {
    view: HostView,
    search: String,
    draft: Option<HostDraft>,
    pending_delete: Option<HostId>,
    message: Option<HostsMessage>,
}

impl HostsPanel {
    /// A panel showing every saved host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current list view.
    #[must_use]
    pub fn view(&self) -> HostView {
        self.view
    }

    /// Switches the list view and clears a pending question, which belonged to what
    /// the user was looking at.
    pub fn select_view(&mut self, view: HostView) {
        if self.view != view {
            self.view = view;
            self.pending_delete = None;
        }
    }

    /// The current search phrase.
    #[must_use]
    pub fn search(&self) -> &str {
        &self.search
    }

    /// Sets the search phrase, which every view applies.
    pub fn set_search(&mut self, phrase: impl Into<String>) {
        self.search = phrase.into();
    }

    /// The open form, if any.
    #[must_use]
    pub fn draft(&self) -> Option<&HostDraft> {
        self.draft.as_ref()
    }

    /// The open form, for the fields to write into.
    pub fn draft_mut(&mut self) -> Option<&mut HostDraft> {
        self.draft.as_mut()
    }

    /// Whether a form is open.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.draft.is_some()
    }

    /// Opens an empty form for a new host.
    pub fn open_create(&mut self) {
        self.draft = Some(HostDraft::create());
        self.pending_delete = None;
        self.clear_message();
    }

    /// Opens the form filled with a saved host.
    pub fn open_edit(&mut self, host: &HostConfig) {
        self.draft = Some(HostDraft::edit(host));
        self.pending_delete = None;
        self.clear_message();
    }

    /// Closes the form, keeping what was typed out of the records.
    pub fn close_editor(&mut self) {
        self.draft = None;
    }

    /// The host the user is about to delete.
    #[must_use]
    pub fn pending_delete(&self) -> Option<HostId> {
        self.pending_delete
    }

    /// Asks the deletion question for one host.
    pub fn ask_delete(&mut self, id: HostId) {
        self.pending_delete = Some(id);
        self.draft = None;
    }

    /// Drops the deletion question without touching the records.
    pub fn cancel_delete(&mut self) {
        self.pending_delete = None;
    }

    /// Takes the confirmed deletion target.
    pub fn take_delete(&mut self) -> Option<HostId> {
        self.pending_delete.take()
    }

    /// The last message, if any.
    #[must_use]
    pub fn message(&self) -> Option<&HostsMessage> {
        self.message.as_ref()
    }

    /// Records what the last action produced.
    pub fn set_message(&mut self, text: impl Into<String>, failed: bool) {
        self.message = Some(HostsMessage {
            text: text.into(),
            failed,
        });
    }

    /// Removes the last message.
    pub fn clear_message(&mut self) {
        self.message = None;
    }

    /// The hosts the current view lists, filtered by the search phrase.
    #[must_use]
    pub fn filtered<'a>(&self, book: &'a HostBook) -> Vec<&'a HostConfig> {
        book.search(&self.search)
    }

    /// The groups the current view lists, filtered by the search phrase.
    #[must_use]
    pub fn groups<'a>(&self, book: &'a HostBook) -> Vec<Group<'a>> {
        HostBook::group(book.search(&self.search))
    }

    /// The reached hosts the recent view lists, filtered by the search phrase.
    #[must_use]
    pub fn recent<'a>(&self, book: &'a HostBook) -> Vec<RecentHost<'a>> {
        book.recent_hosts()
            .into_iter()
            .filter(|recent| host_matches(recent.host, &self.search))
            .collect()
    }
}

/// The import panel: which configuration to read, and what reading it showed.
///
/// The panel holds the result of one read rather than a live view of the file.
/// Changing the path drops the previous preview, so the hosts that can be applied are
/// always the ones that were shown for that path; the file itself is never opened for
/// writing, which is why nothing here has a save path of its own.
#[derive(Debug)]
pub struct ImportPanel {
    open: bool,
    path: String,
    preview: Option<ImportPreview>,
    error: Option<String>,
}

impl Default for ImportPanel {
    fn default() -> Self {
        Self {
            open: false,
            path: DEFAULT_CONFIG_TEXT.to_owned(),
            preview: None,
            error: None,
        }
    }
}

impl ImportPanel {
    /// A closed panel pointing at the file the platform's ssh reads.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the panel is on screen.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Opens the panel on a path, dropping whatever the previous path showed.
    pub fn open(&mut self, path: impl Into<String>) {
        self.open = true;
        self.path = path.into();
        self.preview = None;
        self.error = None;
    }

    /// Closes the panel. The path is kept for the next time it is opened.
    pub fn close(&mut self) {
        self.open = false;
        self.preview = None;
        self.error = None;
    }

    /// The path the user typed.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Replaces the path, dropping a result that belongs to the previous one.
    pub fn set_path(&mut self, path: impl Into<String>) {
        let path = path.into();
        if path != self.path {
            self.path = path;
            self.preview = None;
            self.error = None;
        }
    }

    /// What the last read showed.
    #[must_use]
    pub fn preview(&self) -> Option<&ImportPreview> {
        self.preview.as_ref()
    }

    /// Adopts a preview as the result to show.
    pub fn set_preview(&mut self, preview: ImportPreview) {
        self.preview = Some(preview);
        self.error = None;
    }

    /// Why the file could not be read or parsed.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Reports a read or parse failure, which no preview can survive.
    pub fn set_error(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
        self.preview = None;
    }

    /// Reports a failure that happened after the file was read.
    ///
    /// The preview stays on screen because it is still a true reading of the file, and
    /// the reason says why the hosts could not be written instead.
    pub fn set_apply_error(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
    }

    /// Whether the reason on screen belongs to a read that did produce a preview.
    #[must_use]
    pub fn keeps_preview(&self) -> bool {
        self.preview.is_some()
    }
}

/// The row number the first host of each group starts at.
///
/// The numbering runs across the groups rather than restarting inside each one: two
/// rows of one list must not share an element id, or the hover and active state of one
/// follows the other. Deriving the number here, once, is what keeps a view from
/// passing a constant by hand.
#[must_use]
pub fn group_row_offsets(groups: &[Group<'_>]) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(groups.len());
    let mut next = 0usize;
    for group in groups {
        offsets.push(next);
        next += group.len();
    }
    offsets
}

/// The message an applied import leaves behind.
///
/// It states what was written, how much of it needs a look and how many hosts were
/// skipped, so an import never reads as a plain success when part of it was dropped.
#[must_use]
pub fn import_message(strings: &Strings, imported: usize, review: usize, skipped: usize) -> String {
    format!(
        "{} {imported} {} · {} {review} · {} {skipped}",
        strings.import_applied,
        strings.count_hosts,
        strings.import_review_count,
        strings.import_skipped
    )
}

/// Formats a Unix timestamp as UTC, without pulling a date library into the shell.
///
/// The recent view has to say *when* a host was reached, and a stored integer is not
/// an answer a user can read. The conversion is the standard civil-from-days
/// algorithm, and the tests pin it to known instants.
#[must_use]
pub fn format_unix(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let time = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        time / 3600,
        (time % 3600) / 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::copy::{CHINESE, ENGLISH};
    use shellune_storage::HostBook;

    fn host(name: &str, address: &str, port: u16, user: &str, group: Option<&str>) -> HostConfig {
        let mut host = HostConfig::new(name, address, port, user).expect("a valid host");
        host.set_group(group);
        host
    }

    #[test]
    fn a_new_form_starts_with_the_default_port_and_no_content() {
        let draft = HostDraft::create();
        assert_eq!(draft.text(HostField::Port), "22");
        assert_eq!(draft.text(HostField::Name), "");
        assert_eq!(draft.editing(), None);
        assert_eq!(
            draft.build(None),
            Err("host name cannot be empty".to_owned())
        );
    }

    #[test]
    fn a_port_is_refused_by_the_domain_when_it_is_outside_the_tcp_range() {
        let mut draft = HostDraft::create();
        draft.set_text(HostField::Name, "web");
        draft.set_text(HostField::Address, "10.0.0.5");
        draft.set_text(HostField::User, "deploy");

        draft.set_text(HostField::Port, "0");
        let error = draft.build(None).expect_err("port 0 is not usable");
        assert!(
            error.contains("1-65535"),
            "the reason names the range: {error}"
        );

        draft.set_text(HostField::Port, "70000");
        let error = draft.build(None).expect_err("port 70000 is not usable");
        assert!(
            error.contains("70000"),
            "the reason names the value: {error}"
        );

        draft.set_text(HostField::Port, "ssh");
        let error = draft.build(None).expect_err("a word is not a port");
        assert!(error.contains("ssh"), "the reason names the value: {error}");

        draft.set_text(HostField::Port, "2222");
        let edit = draft.build(None).expect("a valid host");
        assert_eq!(edit.host().port().get(), 2222);
        assert_eq!(edit.host().target(), "deploy@10.0.0.5:2222");
        assert_eq!(edit.replaces(), None);
    }

    #[test]
    fn an_empty_port_field_means_the_ssh_default() {
        let mut draft = HostDraft::create();
        draft.set_text(HostField::Name, "web");
        draft.set_text(HostField::Address, "10.0.0.5");
        draft.set_text(HostField::User, "deploy");
        draft.set_text(HostField::Port, "   ");
        let edit = draft.build(None).expect("an empty port is the default");
        assert_eq!(edit.host().port().get(), 22);
    }

    #[test]
    fn a_blank_group_is_no_group_and_an_edit_keeps_the_identifier() {
        let saved = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
        let mut draft = HostDraft::edit(&saved);
        assert_eq!(draft.editing(), Some(saved.id()));
        assert_eq!(draft.text(HostField::Group), "Prod");

        draft.set_text(HostField::Group, "   ");
        draft.set_text(HostField::Address, "web.example.org");
        let edit = draft.build(Some(&saved)).expect("a valid edit");
        assert_eq!(edit.host().id(), saved.id(), "an edit keeps the identifier");
        assert_eq!(edit.replaces(), Some(saved.id()));
        assert_eq!(edit.host().group(), None);
        assert_eq!(edit.host().address(), "web.example.org");
    }

    #[test]
    fn a_rejected_edit_leaves_the_saved_host_alone() {
        let saved = host("web", "10.0.0.5", 22, "deploy", None);
        let mut draft = HostDraft::edit(&saved);
        draft.set_text(HostField::Port, "0");
        assert!(draft.build(Some(&saved)).is_err());
        assert_eq!(saved.port().get(), 22);
        assert_eq!(
            draft.text(HostField::Port),
            "0",
            "the field keeps what was typed"
        );
    }

    #[test]
    fn the_views_read_the_same_records_and_the_search_filters_all_of_them() {
        let web = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
        let api = host("api", "api.example.org", 2222, "svc", Some("prod"));
        let lab = host("lab", "lab.internal", 22, "root", None);
        let mut book = HostBook::from_hosts(vec![web.clone(), api, lab]).expect("distinct hosts");
        book.record_connection(web.id(), 1_700_000_000)
            .expect("recorded");

        let mut panel = HostsPanel::new();
        assert_eq!(panel.view(), HostView::All);
        assert_eq!(panel.filtered(&book).len(), 3);
        assert_eq!(panel.groups(&book).len(), 2);
        assert_eq!(
            panel.recent(&book).len(),
            1,
            "only a real connection is recent"
        );

        panel.set_search("prod");
        assert_eq!(panel.filtered(&book).len(), 2);
        assert_eq!(panel.groups(&book)[0].len(), 2);
        assert_eq!(
            panel.recent(&book).len(),
            1,
            "the recent view applies the same search"
        );

        panel.set_search("nothing matches this");
        assert!(panel.filtered(&book).is_empty());
        assert!(panel.groups(&book).is_empty());
        assert!(panel.recent(&book).is_empty());
    }

    #[test]
    fn opening_a_form_clears_a_previous_message_and_question() {
        let saved = host("web", "10.0.0.5", 22, "deploy", None);
        let mut panel = HostsPanel::new();
        panel.set_message("Host saved.", false);
        panel.ask_delete(saved.id());
        assert_eq!(panel.pending_delete(), Some(saved.id()));

        panel.open_create();
        assert!(panel.is_editing());
        assert_eq!(panel.pending_delete(), None);
        assert!(panel.message().is_none());

        panel.open_edit(&saved);
        assert_eq!(panel.draft().and_then(HostDraft::editing), Some(saved.id()));
        panel.close_editor();
        assert!(!panel.is_editing());
    }

    #[test]
    fn switching_the_view_drops_a_question_about_the_previous_one() {
        let saved = host("web", "10.0.0.5", 22, "deploy", None);
        let mut panel = HostsPanel::new();
        panel.ask_delete(saved.id());
        panel.select_view(HostView::Recent);
        assert_eq!(panel.view(), HostView::Recent);
        assert_eq!(panel.pending_delete(), None);
    }

    #[test]
    fn unix_seconds_are_shown_as_a_readable_utc_instant() {
        assert_eq!(format_unix(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_unix(1_700_000_000), "2023-11-14 22:13 UTC");
        assert_eq!(format_unix(1_700_000_100), "2023-11-14 22:15 UTC");
        assert_eq!(format_unix(951_782_400), "2000-02-29 00:00 UTC");
    }

    #[test]
    fn the_import_panel_drops_a_result_that_belongs_to_another_path() {
        let mut panel = ImportPanel::new();
        assert!(!panel.is_open());
        assert_eq!(panel.path(), DEFAULT_CONFIG_TEXT);
        assert!(panel.preview().is_none());
        assert!(panel.error().is_none());

        panel.open("C:/Users/example/.ssh/config");
        assert!(panel.is_open());
        assert_eq!(panel.path(), "C:/Users/example/.ssh/config");

        let preview = ImportPreview::from_text(
            "C:/Users/example/.ssh/config",
            "Host web\n    HostName web.example.org\n    User deploy\n",
        )
        .expect("the file parses");
        panel.set_preview(preview);
        assert!(panel.preview().is_some());
        assert!(panel.error().is_none());

        // Editing the path is the start of another question, so the answer to the old
        // one must not stay on screen where it could be applied to the new file.
        panel.set_path("C:/Users/example/other-config");
        assert!(panel.preview().is_none());
        assert!(panel.error().is_none());

        panel.set_error("the file could not be read");
        assert!(panel.error().is_some());
        assert!(panel.preview().is_none());

        panel.close();
        assert!(!panel.is_open());
        assert!(panel.preview().is_none());
        assert_eq!(
            panel.path(),
            "C:/Users/example/other-config",
            "the path is kept for the next time the panel opens"
        );
    }

    #[test]
    fn an_import_message_states_what_was_written_and_what_was_not() {
        let message = import_message(&ENGLISH, 2, 1, 3);
        assert!(message.contains('2'), "{message}");
        assert!(message.contains("Imported"), "{message}");
        assert!(message.contains("need review"), "{message}");
        assert!(message.contains("skipped"), "{message}");

        let chinese = import_message(&CHINESE, 2, 1, 3);
        assert!(chinese.contains("已导入"), "{chinese}");
        assert!(chinese.contains("需复核"), "{chinese}");
    }

    #[test]
    fn the_groups_of_a_list_number_their_rows_across_the_whole_list() {
        let web = host("web", "10.0.0.5", 22, "deploy", Some("Prod"));
        let api = host("api", "api.example.org", 2222, "svc", Some("prod"));
        let lab = host("lab", "lab.internal", 22, "root", None);
        let groups = HostBook::group(vec![&web, &api, &lab]);
        assert_eq!(groups.len(), 2, "one named group and the ungrouped one");
        assert_eq!(
            group_row_offsets(&groups),
            vec![0, 2],
            "each group starts where the previous one ended, so no two rows share an index"
        );
        assert!(group_row_offsets(&[]).is_empty());
    }

    #[test]
    fn a_draft_keeps_an_ordered_jump_chain_and_clears_a_blank_proxy_command() {
        let bastion = host("bastion", "10.0.0.1", 22, "jump", None);
        let second = host("edge", "10.0.0.3", 22, "jump", None);
        let web = host("web", "10.0.0.2", 22, "deploy", None);
        let mut draft = HostDraft::edit(&web);
        assert!(draft.jumps().is_empty());
        assert_eq!(draft.text(HostField::ProxyCommand), "");
        assert!(draft.add_jump(bastion.id()));
        assert!(draft.add_jump(second.id()));
        assert!(!draft.add_jump(web.id()), "a host is not a jump to itself");
        assert!(!draft.add_jump(bastion.id()), "a hop is listed once");
        draft.move_jump(1, true);
        assert_eq!(draft.jumps(), &[second.id(), bastion.id()]);
        draft.set_text(HostField::ProxyCommand, "  nc %h %p  ");
        assert!(draft.proxy_conflicts());
        let edit = draft.build(Some(&web)).expect("the host is valid");
        let saved = edit.host();
        assert_eq!(saved.jumps(), &[second.id(), bastion.id()]);
        assert_eq!(saved.proxy_command(), Some("nc %h %p"));

        let mut again = HostDraft::edit(saved);
        assert_eq!(again.jumps(), &[second.id(), bastion.id()]);
        again.remove_jump(0);
        again.set_text(HostField::ProxyCommand, "   ");
        let cleared = again
            .build(Some(saved))
            .expect("clearing the command is valid");
        assert_eq!(cleared.host().jumps(), &[bastion.id()]);
        assert_eq!(cleared.host().proxy_command(), None);
        assert!(!HostDraft::edit(cleared.host()).proxy_conflicts());
    }
}
