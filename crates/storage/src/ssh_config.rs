//! Importing a user's SSH configuration without overstating what was understood
//! (R5, R14-R16).
//!
//! The importer reads one file, never writes it, and returns a preview the shell can
//! show before anything is saved. OpenSSH itself is the reference for the subset it
//! claims:
//!
//! * a host comes from a concrete Host alias, that is one spelled without a wildcard
//!   and without a negation;
//! * HostName, User, Port and IdentityFile follow OpenSSH first-obtained-value: the
//!   first line that sets a keyword for an alias wins, and a later line that would
//!   have changed it is recorded as superseded;
//! * Host patterns are matched against the alias with the same wildcard syntax, so a
//!   "Host *" block does apply to every alias while "Host web*" only reaches the
//!   aliases it matches, and a leading "!" excludes;
//! * a host that never sets HostName is reached under the alias itself, exactly as
//!   ssh does it.
//!
//! A proxy or a forwarding in a block whose pattern no alias in the file matches is
//! reported too: it reached no imported host, but ssh would still apply it to a name
//! this file never defines, so the result is partial rather than complete.
//!
//! Everything the subset cannot decide is reported instead of guessed: a Match
//! block, an Include, an OpenSSH token this build cannot expand, and a directive
//! this build does not apply. ProxyJump, ProxyCommand, LocalForward, RemoteForward
//! and DynamicForward are read into the preview. A rule that can be stored as a
//! jump, a command or a forwarding rule is offered with the host. A rule that is
//! incomplete, conditional, or uses a token the connection layer does not expand
//! stays visible and is not applied, so a direct connection is never substituted
//! for a path the file did not describe.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use shellune_core::{
    DEFAULT_SSH_PORT, Endpoint, ForwardBuildError, ForwardKind, ForwardRule, HostConfig,
    HostConnection, HostId, X11Forwarding,
};

use crate::book::{HostBook, HostEdit};
use crate::error::BookError;

/// The file a user's SSH client reads when no other file is named.
///
/// Shown as the default in the import panel; the tilde is resolved by
/// [`resolve_path`] before anything is opened.
pub const DEFAULT_CONFIG_TEXT: &str = "~/.ssh/config";

/// Why a configuration could not be turned into a preview at all.
///
/// Both variants name the file, and a syntax failure names the line, because "the
/// import failed" without a location is not something a user can act on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// The file exists but could not be read as text.
    #[error("the SSH configuration at {path} could not be read: {reason}")]
    Unreadable {
        /// The file that was attempted.
        path: PathBuf,
        /// What the operating system or the encoding check reported.
        reason: String,
    },
    /// The file was read but one line is not a statement this importer understands.
    #[error("the SSH configuration at {path} line {line} could not be parsed: {reason}")]
    Syntax {
        /// The file that is malformed.
        path: PathBuf,
        /// The one-based line number of the offending line.
        line: usize,
        /// What is wrong with the line.
        reason: String,
    },
}

impl ImportError {
    /// The file the failure is about.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Unreadable { path, .. } | Self::Syntax { path, .. } => path,
        }
    }

    /// The line the failure is about, when it is about one line.
    #[must_use]
    pub fn line(&self) -> Option<usize> {
        match self {
            Self::Unreadable { .. } => None,
            Self::Syntax { line, .. } => Some(*line),
        }
    }

    /// The reason, without the location prefix.
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Unreadable { reason, .. } | Self::Syntax { reason, .. } => reason,
        }
    }
}

/// How one host from the file would be imported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    /// Every setting that reached this host is understood, and nothing was skipped.
    Ready,
    /// The host can be imported, but part of the file could not be applied to it.
    NeedsReview,
    /// The file describes a path or a forward this build cannot store completely.
    ///
    /// The host is not imported. Saving it as a direct connection would drop a jump,
    /// a command or a rule the preview could not express.
    Blocked,
}

impl ImportOutcome {
    /// Whether the host may be offered for import at all.
    #[must_use]
    pub fn is_offered(self) -> bool {
        !matches!(self, Self::Blocked)
    }
}

/// What kind of limitation one note reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteKind {
    /// A jump host or a proxy command.
    Proxy,
    /// A local, remote or dynamic forwarding.
    Forwarding,
    /// An Include, whose files were not read.
    Include,
    /// A Match block, whose conditions were not evaluated.
    Match,
    /// An OpenSSH token such as %h, which this importer does not expand.
    Token,
    /// A directive this build does not apply.
    Unsupported,
    /// A value this build cannot use, kept so the line can be pointed at.
    InvalidValue,
    /// A line whose value a later line could not override.
    Superseded,
    /// A field the record needs and the file never sets.
    MissingField,
}

impl NoteKind {
    /// Whether this kind of note, by itself, used to mean the host could not be saved.
    ///
    /// A proxy or a forward that was fully read does not block the host. Only an
    /// incomplete or conditional one does, and that decision is the host's outcome,
    /// not the mere presence of the keyword.
    #[must_use]
    pub fn blocks_import(self) -> bool {
        false
    }
}

/// One limitation, with the line that caused it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportNote {
    line: usize,
    keyword: String,
    kind: NoteKind,
    reason: String,
}

impl ImportNote {
    fn new(
        line: usize,
        keyword: impl Into<String>,
        kind: NoteKind,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            line,
            keyword: keyword.into(),
            kind,
            reason: reason.into(),
        }
    }

    /// One-based line the note is about.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    /// The directive, or the Host pattern a block-level note is about, as it was
    /// spelled in the file.
    #[must_use]
    pub fn keyword(&self) -> &str {
        &self.keyword
    }

    /// What kind of limitation this is.
    #[must_use]
    pub fn kind(&self) -> NoteKind {
        self.kind
    }

    /// The reason, in the words of the importer.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// One hop a ProxyJump line named.
///
/// `alias` is set when the hop names a Host block in the same file. Otherwise the
/// hop is a literal `user@address` and the plan creates a host for it. Reading this
/// value does not open a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpSpec {
    alias: Option<String>,
    user: Option<String>,
    address: String,
    port: Option<u16>,
}

impl JumpSpec {
    /// The Host alias this hop uses, when the file named one.
    #[must_use]
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }

    /// The login user, when the line or the alias decided one.
    #[must_use]
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// The address or alias text the line named.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The port, when the line named one. Absent means the hop's own port.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// How the preview names the hop.
    #[must_use]
    pub fn label(&self) -> String {
        match (&self.user, self.port) {
            (Some(user), Some(port)) => format!("{user}@{}:{port}", self.address),
            (Some(user), None) => format!("{user}@{}", self.address),
            (None, Some(port)) => format!("{}:{port}", self.address),
            (None, None) => self.address.clone(),
        }
    }
}

/// One forwarding rule the file described completely enough to store.
///
/// It is configuration only. Building it does not bind a port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedForward {
    kind: ForwardKind,
    listen_host: String,
    listen_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
    line: usize,
}

impl ImportedForward {
    /// Which tunnel the line asked for.
    #[must_use]
    pub fn kind(&self) -> ForwardKind {
        self.kind
    }

    /// Address the listener would bind. A bare port in the file becomes the loopback.
    #[must_use]
    pub fn listen_host(&self) -> &str {
        &self.listen_host
    }

    /// Port the listener would bind.
    #[must_use]
    pub fn listen_port(&self) -> u16 {
        self.listen_port
    }

    /// Where traffic would go, absent for a dynamic rule.
    #[must_use]
    pub fn target_host(&self) -> Option<&str> {
        self.target_host.as_deref()
    }

    /// Target port, absent for a dynamic rule.
    #[must_use]
    pub fn target_port(&self) -> Option<u16> {
        self.target_port
    }

    /// One-based line of the directive.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    /// One line for the preview.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.kind {
            ForwardKind::Dynamic => {
                format!("dynamic {}:{}", self.listen_host, self.listen_port)
            }
            ForwardKind::Local | ForwardKind::Remote => format!(
                "{} {}:{} → {}:{}",
                self.kind.label(),
                self.listen_host,
                self.listen_port,
                self.target_host.as_deref().unwrap_or("?"),
                self.target_port.unwrap_or(0)
            ),
        }
    }

    /// The saved rule this line becomes, bound to `host`.
    ///
    /// # Errors
    ///
    /// Returns the endpoint error when an address or port would not survive a save.
    pub fn rule(&self, host: HostId) -> Result<ForwardRule, ForwardBuildError> {
        let listen = Endpoint::new(&self.listen_host, self.listen_port)?;
        match self.kind {
            ForwardKind::Dynamic => Ok(ForwardRule::dynamic(host, listen)),
            ForwardKind::Local | ForwardKind::Remote => {
                let target_host = self.target_host.as_deref().unwrap_or("");
                let target_port = self.target_port.unwrap_or(0);
                let target = Endpoint::new(target_host, target_port)?;
                if self.kind == ForwardKind::Local {
                    Ok(ForwardRule::local(host, listen, target))
                } else {
                    Ok(ForwardRule::remote(host, listen, target))
                }
            }
        }
    }
}

/// One host the file describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedHost {
    alias: String,
    address: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    identity_file: Option<String>,
    /// ProxyJump hops the file named. Present even when the host is not imported,
    /// so the preview can show the path it refused to guess.
    jumps: Vec<JumpSpec>,
    /// True once a ProxyJump line, including `none`, has been taken.
    jump_set: bool,
    /// Line of the ProxyJump that was kept, for a later note.
    jump_line: usize,
    /// ProxyCommand text. Present even when it is not safe to store.
    proxy_command: Option<String>,
    /// True once a ProxyCommand line, including `none`, has been taken.
    proxy_set: bool,
    forwards: Vec<ImportedForward>,
    /// The X11 mode the file's ForwardX11/ForwardX11Trusted lines add up to (R11).
    x11: Option<X11Forwarding>,
    /// The first ForwardX11 value the file set, kept so the trusted line can be read
    /// in its context: a trusted yes enables forwarding even without ForwardX11 yes.
    forward_x11: Option<bool>,
    /// The first ForwardX11Trusted value the file set.
    forward_x11_trusted: Option<bool>,
    /// The line of the first X11 directive, for a note about a bad value.
    x11_line: usize,
    /// The first `ServerAliveInterval` the file set, in seconds (R20).
    ///
    /// `0` is OpenSSH's way of switching the probe off and is kept as written, so the
    /// preview can say the file turns keepalive off instead of quietly inheriting.
    alive_interval: Option<u32>,
    /// The line of that interval, for a superseded or unusable value.
    alive_interval_line: usize,
    /// The first `ServerAliveCountMax` the file set (R20).
    alive_count: Option<u32>,
    /// The line of that count.
    alive_count_line: usize,
    /// The network fields must be shown and must not be written.
    withhold: bool,
    outcome: ImportOutcome,
    notes: Vec<ImportNote>,
}

impl ImportedHost {
    /// The alias the Host line named.
    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// The address a connection would dial, when the file decided one.
    #[must_use]
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }

    /// The port a connection would dial: what the file sets, else the SSH default.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(DEFAULT_SSH_PORT)
    }

    /// The user a connection would log in as, when the file set one.
    #[must_use]
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// The private key path the file names, when it names one.
    #[must_use]
    pub fn identity_file(&self) -> Option<&str> {
        self.identity_file.as_deref()
    }

    /// ProxyJump hops, in order. Empty when the file sets none, or sets `none`.
    #[must_use]
    pub fn jumps(&self) -> &[JumpSpec] {
        &self.jumps
    }

    /// ProxyCommand text, when the file sets one other than `none`.
    ///
    /// The text is what a later confirmed connection would run. Importing it does
    /// not run it.
    #[must_use]
    pub fn proxy_command(&self) -> Option<&str> {
        self.proxy_command.as_deref()
    }

    /// Forwarding rules the file described completely.
    #[must_use]
    pub fn forwards(&self) -> &[ImportedForward] {
        &self.forwards
    }

    /// The X11 mode the file asks for, when it asks for one (R11).
    ///
    /// None means the file says nothing about X11, which is the off default. The
    /// trusted mode is what ForwardX11Trusted yes means and still needs the risk
    /// confirmation before a connection uses it.
    #[must_use]
    pub fn x11(&self) -> Option<X11Forwarding> {
        self.x11
    }

    /// The keepalive this host's lines add up to, when the file sets one (R20).
    ///
    /// `None` means the file says nothing about keepalive, which is inheritance: the
    /// global default applies, exactly as it does for a host saved before the choice
    /// existed. An interval of `0` is an explicit disable and is kept.
    #[must_use]
    pub fn connection(&self) -> Option<HostConnection> {
        HostConnection::new(self.alive_interval, self.alive_count, None)
            .ok()
            .filter(|connection| !connection.is_empty())
    }

    /// How this host would be imported.
    #[must_use]
    pub fn outcome(&self) -> ImportOutcome {
        self.outcome
    }

    /// Every limitation recorded for this host, in the order it was found.
    #[must_use]
    pub fn notes(&self) -> &[ImportNote] {
        &self.notes
    }

    /// Whether the record could really be created from this host.
    ///
    /// A host is offered when nothing blocks it and the record would carry what a
    /// connection needs. A host that is missing its user is not offered: the
    /// application stores an explicit user, and inventing one would be a guess
    /// about who the user logs in as.
    #[must_use]
    pub fn is_importable(&self) -> bool {
        self.outcome.is_offered() && !self.withhold && self.user.is_some() && self.address.is_some()
    }

    /// The target as a confirmation prompt would name it.
    #[must_use]
    pub fn target(&self) -> String {
        let address = self.address.as_deref().unwrap_or(&self.alias);
        match &self.user {
            Some(user) => format!("{user}@{address}:{}", self.port()),
            None => format!("{address}:{}", self.port()),
        }
    }

    /// A one-line summary of what would be saved, for a preview row.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut summary = self.target();
        if let Some(key) = &self.identity_file {
            summary.push_str(" · ");
            summary.push_str(key);
        }
        if !self.jumps.is_empty() {
            let hops: Vec<String> = self.jumps.iter().map(JumpSpec::label).collect();
            summary.push_str(" · jump ");
            summary.push_str(&hops.join(", "));
        }
        if let Some(command) = &self.proxy_command {
            summary.push_str(" · ");
            summary.push_str(command);
        }
        if !self.forwards.is_empty() {
            summary.push_str(" · ");
            summary.push_str(&format!("{} forward", self.forwards.len()));
        }
        if let Some(connection) = self.connection() {
            summary.push_str(" · ");
            summary.push_str(&describe_connection(connection));
        }
        match self.x11 {
            Some(X11Forwarding::Off) => summary.push_str(" · X11 off"),
            Some(X11Forwarding::Restricted) => summary.push_str(" · X11"),
            Some(X11Forwarding::Trusted) => summary.push_str(" · X11 trusted"),
            None => {}
        }
        summary
    }
}

/// What the file could not contribute at all.
///
/// These are the limitations that belong to the whole file rather than to one host,
/// and the preview shows them before the user applies anything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportLimits {
    ignored: Vec<ImportNote>,
    include_lines: Vec<usize>,
    match_lines: Vec<usize>,
    unmatched_proxy: Vec<ImportNote>,
}

impl ImportLimits {
    /// Directives this build does not apply, one entry per keyword.
    #[must_use]
    pub fn ignored(&self) -> &[ImportNote] {
        &self.ignored
    }

    /// Lines holding an Include, whose files were not read.
    #[must_use]
    pub fn include_lines(&self) -> &[usize] {
        &self.include_lines
    }

    /// Lines starting a Match block, whose conditions were not evaluated.
    #[must_use]
    pub fn match_lines(&self) -> &[usize] {
        &self.match_lines
    }

    /// Proxy and forwarding directives in a Host block that no host in this file
    /// matches, one entry per block.
    ///
    /// A pattern such as `*.corp` or a negation decides which aliases a block applies
    /// to. When no concrete alias in the file is covered, the directives never reach
    /// an imported host, so without this list they would vanish from the preview while
    /// ssh would still use them for `ssh box.corp`.
    #[must_use]
    pub fn unmatched_proxy(&self) -> &[ImportNote] {
        &self.unmatched_proxy
    }

    /// Whether the file as a whole was only partly applied.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        !self.ignored.is_empty()
            || !self.include_lines.is_empty()
            || !self.match_lines.is_empty()
            || !self.unmatched_proxy.is_empty()
    }
}

/// What a preview would change in the saved hosts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportPlan {
    edits: Vec<HostEdit>,
    skipped: Vec<SkippedHost>,
    forwards: Vec<PlannedForward>,
}

/// One forwarding rule to create for a host the plan also creates or already saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedForward {
    alias: String,
    forward: ImportedForward,
}

impl PlannedForward {
    /// The host alias the rule belongs to.
    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// The rule, not yet bound to a host id.
    #[must_use]
    pub fn forward(&self) -> &ImportedForward {
        &self.forward
    }
}

impl ImportPlan {
    /// The records to create.
    #[must_use]
    pub fn edits(&self) -> &[HostEdit] {
        &self.edits
    }

    /// The hosts that were not offered, with the reason.
    #[must_use]
    pub fn skipped(&self) -> &[SkippedHost] {
        &self.skipped
    }

    /// Forwarding rules that belong to hosts this plan can save.
    #[must_use]
    pub fn forwards(&self) -> &[PlannedForward] {
        &self.forwards
    }

    /// Whether the plan would change nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// How many records the plan would create.
    #[must_use]
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    /// Applies every edit to the given book.
    ///
    /// Callers pass a copy they are about to save: a failure part-way leaves the copy
    /// changed, and only the file is the authority on what is really saved.
    ///
    /// # Errors
    ///
    /// Returns the book error of the first edit that could not be applied, which can
    /// only happen if the plan was built against a different book.
    pub fn apply(&self, book: &mut HostBook) -> Result<usize, BookError> {
        let mut applied = 0;
        for edit in &self.edits {
            book.apply(edit)?;
            applied += 1;
        }
        Ok(applied)
    }
}

/// One host from the file that will not be imported, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedHost {
    alias: String,
    reason: String,
}

impl SkippedHost {
    /// The alias that was not imported.
    #[must_use]
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Why it was not imported.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// What one file would contribute, before anything is saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPreview {
    source: PathBuf,
    hosts: Vec<ImportedHost>,
    limits: ImportLimits,
}

impl ImportPreview {
    /// Reads a configuration file and previews it.
    ///
    /// The file is opened for reading and for nothing else: the importer has no
    /// write path at all, so an import cannot change what the SSH client itself
    /// reads.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::Unreadable`] when the path is a directory, cannot be
    /// opened or is not UTF-8, and [`ImportError::Syntax`] when a line is not a
    /// statement this importer understands. Neither case yields a preview, so a
    /// failure is never shown as a partial success.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ImportError> {
        let path = resolve_path(&path.as_ref().to_string_lossy());
        if path.is_dir() {
            return Err(ImportError::Unreadable {
                path,
                reason: "it is a directory, not a configuration file".to_owned(),
            });
        }
        let text = std::fs::read_to_string(&path).map_err(|error| ImportError::Unreadable {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        Self::from_text(path, &text)
    }

    /// Previews configuration text that was already read.
    ///
    /// The source path is kept for display and for the record of where the hosts
    /// came from; it is never opened.
    ///
    /// # Errors
    ///
    /// Returns [`ImportError::Syntax`] when a line is not a statement this importer
    /// understands.
    pub fn from_text(source: impl Into<PathBuf>, text: &str) -> Result<Self, ImportError> {
        let source = source.into();
        let blocks = parse_blocks(&source, text)?;
        let aliases = concrete_aliases(&blocks);
        let limits = file_limits(&blocks, &aliases);
        let mut hosts: Vec<ImportedHost> = aliases
            .into_iter()
            .map(|(alias, line)| resolve_host(&blocks, &alias, line))
            .collect();
        finalize_jumps(&mut hosts);
        Ok(Self {
            source,
            hosts,
            limits,
        })
    }

    /// The file the hosts came from.
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Every host the file describes, in file order.
    #[must_use]
    pub fn hosts(&self) -> &[ImportedHost] {
        &self.hosts
    }

    /// What the file could not contribute at all.
    #[must_use]
    pub fn limits(&self) -> &ImportLimits {
        &self.limits
    }

    /// The hosts that could really be imported.
    pub fn importable(&self) -> impl Iterator<Item = &ImportedHost> {
        self.hosts.iter().filter(|host| host.is_importable())
    }

    /// How many hosts could be imported.
    #[must_use]
    pub fn importable_count(&self) -> usize {
        self.importable().count()
    }

    /// How many hosts can be imported but need a review first.
    #[must_use]
    pub fn review_count(&self) -> usize {
        self.hosts
            .iter()
            .filter(|host| host.outcome == ImportOutcome::NeedsReview && host.is_importable())
            .count()
    }

    /// How many hosts are not imported because a proxy or a forwarding could apply.
    #[must_use]
    pub fn blocked_count(&self) -> usize {
        self.hosts
            .iter()
            .filter(|host| host.outcome == ImportOutcome::Blocked)
            .count()
    }

    /// Whether the file was only partly applied, either as a whole or host by host.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.limits.is_partial()
            || self
                .hosts
                .iter()
                .any(|host| host.outcome != ImportOutcome::Ready || !host.is_importable())
    }

    /// What this preview would add to the given book.
    ///
    /// A host whose name is already saved is skipped with a reason rather than
    /// duplicated: two records with the same name would make the list ambiguous, and
    /// the saved record is the one the user can already edit. Forwarding rules for
    /// that host are still planned, so a save that stored the host and missed the
    /// rules can write the missing rules on the next apply.
    #[must_use]
    pub fn plan(&self, book: &HostBook) -> ImportPlan {
        let mut taken: HashSet<String> = book
            .hosts()
            .iter()
            .map(|host| host.name().to_lowercase())
            .collect();
        let mut edits = Vec::new();
        let mut skipped = Vec::new();
        let mut created: HashMap<String, HostId> = HashMap::new();
        let mut forwards = Vec::new();

        for host in &self.hosts {
            if !host.is_importable() {
                continue;
            }
            let alias = host.alias();
            let name = alias.to_lowercase();
            if taken.contains(&name) {
                skipped.push(SkippedHost {
                    alias: alias.to_owned(),
                    reason: format!("a saved host is already named {alias}"),
                });
                for forward in host.forwards() {
                    forwards.push(PlannedForward {
                        alias: alias.to_owned(),
                        forward: forward.clone(),
                    });
                }
                continue;
            }
            let Some(user) = host.user() else {
                continue;
            };
            let Some(address) = host.address() else {
                continue;
            };
            let mut record = match HostConfig::new(alias, address, host.port(), user) {
                Ok(record) => record,
                Err(error) => {
                    skipped.push(SkippedHost {
                        alias: alias.to_owned(),
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            record.set_identity_file(host.identity_file());
            record.set_proxy_command(host.proxy_command());
            if let Some(mode) = host.x11() {
                record.set_x11(mode);
            }
            // The keepalive the file asked for is the keepalive the record is saved
            // with, so the preview, the saved host and the next connection agree.
            if let Some(connection) = host.connection() {
                record.set_connection(Some(connection));
            }
            created.insert(name.clone(), record.id());
            taken.insert(name);
            edits.push(HostEdit::create(record));
            for forward in host.forwards() {
                forwards.push(PlannedForward {
                    alias: alias.to_owned(),
                    forward: forward.clone(),
                });
            }
        }

        // Literal jump targets that are not Host blocks of their own.
        for host in &self.hosts {
            if !host.is_importable() {
                continue;
            }
            for jump in host.jumps() {
                if jump.alias().is_some() {
                    continue;
                }
                let Some(user) = jump.user() else {
                    continue;
                };
                let label = jump.label();
                let name = label.to_lowercase();
                if taken.contains(&name) || created.contains_key(&name) {
                    continue;
                }
                let port = jump.port().unwrap_or(DEFAULT_SSH_PORT);
                let Ok(record) = HostConfig::new(&label, jump.address(), port, user) else {
                    continue;
                };
                created.insert(name.clone(), record.id());
                taken.insert(name);
                edits.push(HostEdit::create(record));
            }
        }

        let mut drop_aliases = Vec::new();
        let mut jump_updates: Vec<(String, Vec<HostId>)> = Vec::new();
        for edit in &edits {
            let Some(imported) = self
                .hosts
                .iter()
                .find(|host| host.alias().eq_ignore_ascii_case(edit.host().name()))
            else {
                continue;
            };
            if imported.jumps().is_empty() {
                continue;
            }
            let mut hop_ids = Vec::new();
            let mut unresolved = false;
            for jump in imported.jumps() {
                let Some(id) = jump_id(jump, book, &created) else {
                    unresolved = true;
                    break;
                };
                hop_ids.push(id);
            }
            if unresolved {
                skipped.push(SkippedHost {
                    alias: imported.alias().to_owned(),
                    reason: format!(
                        "the jump chain for {} names a host this import cannot save",
                        imported.alias()
                    ),
                });
                drop_aliases.push(imported.alias().to_owned());
                continue;
            }
            jump_updates.push((imported.alias().to_owned(), hop_ids));
        }
        if !drop_aliases.is_empty() {
            edits.retain(|edit| {
                !drop_aliases
                    .iter()
                    .any(|alias| edit.host().name().eq_ignore_ascii_case(alias))
            });
            forwards.retain(|forward| {
                !drop_aliases
                    .iter()
                    .any(|alias| forward.alias().eq_ignore_ascii_case(alias))
            });
        }
        for edit in &mut edits {
            let Some((_, hop_ids)) = jump_updates
                .iter()
                .find(|(alias, _)| edit.host().name().eq_ignore_ascii_case(alias))
            else {
                continue;
            };
            let mut host = edit.host().clone();
            host.set_jumps(hop_ids.iter().copied());
            *edit = HostEdit::create(host);
        }

        ImportPlan {
            edits,
            skipped,
            forwards,
        }
    }
}

/// The saved id a jump hop refers to: an alias just created, a literal just created,
/// or a host the book already has under that name.
fn jump_id(jump: &JumpSpec, book: &HostBook, created: &HashMap<String, HostId>) -> Option<HostId> {
    let key = match jump.alias() {
        Some(alias) => alias.to_lowercase(),
        None => jump.label().to_lowercase(),
    };
    if let Some(id) = created.get(&key) {
        return Some(*id);
    }
    let name = jump.alias().unwrap_or("");
    let literal = jump.label();
    book.hosts()
        .iter()
        .find(|host| {
            if jump.alias().is_some() {
                host.name().eq_ignore_ascii_case(name)
            } else {
                host.name().eq_ignore_ascii_case(&literal)
            }
        })
        .map(HostConfig::id)
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// One directive line.
///
/// The keyword is kept as it was spelled, so a preview can quote the file back to
/// the reader, and lowered separately, so classification ignores the case.
struct Setting {
    keyword: String,
    lower: String,
    value: String,
    line: usize,
}

/// One Host block, one Match block, or the directives before the first Host line.
struct Block {
    patterns: Vec<Pattern>,
    /// A Match block, whose conditions this importer does not evaluate.
    matches_conditions: bool,
    line: usize,
    settings: Vec<Setting>,
}

/// One entry of a Host line.
struct Pattern {
    text: String,
    negated: bool,
}

impl Pattern {
    fn new(token: &str) -> Self {
        let (negated, text) = match token.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, token),
        };
        Self {
            text: text.to_ascii_lowercase(),
            negated,
        }
    }
}

impl Block {
    fn global(line: usize) -> Self {
        Self {
            patterns: Vec::new(),
            matches_conditions: false,
            line,
            settings: Vec::new(),
        }
    }

    fn host(patterns: Vec<Pattern>, line: usize) -> Self {
        Self {
            patterns,
            matches_conditions: false,
            line,
            settings: Vec::new(),
        }
    }

    fn matched(line: usize) -> Self {
        Self {
            patterns: Vec::new(),
            matches_conditions: true,
            line,
            settings: Vec::new(),
        }
    }

    /// Whether this block applies to an alias.
    ///
    /// A block without patterns is the part of the file before the first Host line
    /// and applies to every host; otherwise a positive pattern has to match and no
    /// negated pattern may, which is how ssh reads a Host line.
    fn covers(&self, alias: &str) -> bool {
        if self.patterns.is_empty() {
            return true;
        }
        let mut positive = false;
        for pattern in &self.patterns {
            if pattern.negated {
                if glob_match(&pattern.text, alias) {
                    return false;
                }
            } else if glob_match(&pattern.text, alias) {
                positive = true;
            }
        }
        positive
    }
}

/// Splits a configuration into statements, refusing anything it cannot read.
fn parse_blocks(source: &Path, text: &str) -> Result<Vec<Block>, ImportError> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut current = Block::global(0);

    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (keyword, value) = split_statement(trimmed).ok_or_else(|| ImportError::Syntax {
            path: source.to_owned(),
            line,
            reason: format!("{trimmed:?} is not a keyword followed by a value"),
        })?;
        let key = keyword.to_ascii_lowercase();
        match key.as_str() {
            "host" => {
                if value.is_empty() {
                    return Err(ImportError::Syntax {
                        path: source.to_owned(),
                        line,
                        reason: "Host needs at least one alias".to_owned(),
                    });
                }
                let patterns: Vec<Pattern> = value
                    .split_whitespace()
                    .map(Pattern::new)
                    .filter(|pattern| !pattern.text.is_empty())
                    .collect();
                if patterns.is_empty() {
                    return Err(ImportError::Syntax {
                        path: source.to_owned(),
                        line,
                        reason: "Host needs at least one alias".to_owned(),
                    });
                }
                blocks.push(std::mem::replace(&mut current, Block::host(patterns, line)));
            }
            "match" => {
                if value.is_empty() {
                    return Err(ImportError::Syntax {
                        path: source.to_owned(),
                        line,
                        reason: "Match needs a condition".to_owned(),
                    });
                }
                blocks.push(std::mem::replace(&mut current, Block::matched(line)));
            }
            _ => {
                if value.is_empty() {
                    return Err(ImportError::Syntax {
                        path: source.to_owned(),
                        line,
                        reason: format!("{keyword} has no value"),
                    });
                }
                current.settings.push(Setting {
                    keyword,
                    lower: key,
                    value,
                    line,
                });
            }
        }
    }
    blocks.push(current);
    Ok(blocks)
}

/// Splits one line into its keyword and its value.
///
/// Both the "Keyword value" and the "Keyword=value" spelling are accepted, the
/// keyword never contains an equals sign, and a value quoted as a whole loses its
/// quotes so a path with a space survives.
fn split_statement(line: &str) -> Option<(String, String)> {
    let equals = line.find('=');
    let space = line.find([' ', '\t']);
    let cut = match (equals, space) {
        (Some(equals), Some(space)) if equals < space => equals + 1,
        (Some(equals), None) => equals + 1,
        (_, Some(space)) => space + 1,
        (None, None) => return None,
    };
    let keyword = line.get(..cut - 1)?.trim().to_owned();
    if keyword.is_empty() {
        return None;
    }
    let mut rest = line.get(cut..)?.trim();
    if let Some(stripped) = rest.strip_prefix('=') {
        rest = stripped.trim_start();
    }
    let value = unquote(rest)?;
    Some((keyword, value))
}

/// Removes one pair of surrounding quotes, refusing a line that is half quoted.
fn unquote(text: &str) -> Option<String> {
    if let Some(inner) = text.strip_prefix('"') {
        let inner = inner.strip_suffix('"')?;
        if inner.contains('"') {
            return None;
        }
        return Some(inner.to_owned());
    }
    // A quote in the middle of a value is not quoting anything this importer has to
    // interpret: a Match condition or a proxy command may contain one, and the value
    // is passed on exactly as it was written.
    Some(text.to_owned())
}

/// Every alias the file defines concretely, with the line that first named it.
///
/// A pattern with a wildcard or a negation does not define a host: it only decides
/// which of the concrete aliases a block applies to.
fn concrete_aliases(blocks: &[Block]) -> Vec<(String, usize)> {
    let mut aliases: Vec<(String, usize)> = Vec::new();
    for block in blocks {
        if block.matches_conditions {
            continue;
        }
        for pattern in &block.patterns {
            if pattern.negated || pattern.text.contains(['*', '?']) {
                continue;
            }
            if aliases
                .iter()
                .any(|(alias, _)| alias.eq_ignore_ascii_case(&pattern.text))
            {
                continue;
            }
            aliases.push((pattern.text.clone(), block.line));
        }
    }
    aliases
}

/// What the file cannot contribute as a whole.
///
/// The aliases are the ones the file defines concretely, and they decide whether a
/// Host block reaches any host at all: a block no alias matches carries directives
/// that the preview would otherwise drop without a word.
fn file_limits(blocks: &[Block], aliases: &[(String, usize)]) -> ImportLimits {
    let mut ignored: BTreeMap<String, ImportNote> = BTreeMap::new();
    let mut include_lines = Vec::new();
    let mut match_lines = Vec::new();
    let mut unmatched_proxy = Vec::new();

    for block in blocks {
        if block.matches_conditions {
            match_lines.push(block.line);
        }
        for setting in &block.settings {
            match classify(&setting.lower) {
                Directive::Include => include_lines.push(setting.line),
                Directive::Ignored => {
                    ignored.entry(setting.lower.clone()).or_insert_with(|| {
                        ImportNote::new(
                            setting.line,
                            setting.keyword.clone(),
                            NoteKind::Unsupported,
                            format!(
                                "line {}: {} is not applied by Shellune, so this part of the configuration was skipped",
                                setting.line, setting.keyword
                            ),
                        )
                    });
                }
                _ => {}
            }
        }

        // A proxy or a forwarding inside a Host block that no concrete alias matches
        // is never resolved onto a host, so nothing else in the preview mentions it.
        // ssh still applies those rules by pattern (`ssh box.corp` would take the
        // jump host), so the file was not read completely and the preview says which
        // pattern and line it skipped instead of claiming the opposite.
        if block.patterns.is_empty() {
            continue;
        }
        let blocking: Vec<&Setting> = block
            .settings
            .iter()
            .filter(|setting| {
                matches!(
                    classify(&setting.lower),
                    Directive::Proxy | Directive::Forwarding
                )
            })
            .collect();
        if blocking.is_empty() {
            continue;
        }
        if aliases
            .iter()
            .any(|(alias, _)| block.covers(alias.as_str()))
        {
            continue;
        }
        let pattern = pattern_text(&block.patterns);
        let directives = blocking
            .iter()
            .map(|setting| format!("{} on line {}", setting.keyword, setting.line))
            .collect::<Vec<String>>()
            .join(", ");
        let kind = if blocking
            .iter()
            .any(|setting| matches!(classify(&setting.lower), Directive::Proxy))
        {
            NoteKind::Proxy
        } else {
            NoteKind::Forwarding
        };
        unmatched_proxy.push(ImportNote::new(
            block.line,
            pattern.clone(),
            kind,
            format!(
                "line {}: Host {pattern} sets {directives}, but no Host in this file matches \
                 that pattern, so the rule was not applied to any imported host and a \
                 connection it would have redirected is not shown here",
                block.line
            ),
        ));
    }

    include_lines.sort_unstable();
    include_lines.dedup();
    ImportLimits {
        ignored: ignored.into_values().collect(),
        include_lines,
        match_lines,
        unmatched_proxy,
    }
}

/// How a Host line spelled its patterns, so a note can quote it back.
fn pattern_text(patterns: &[Pattern]) -> String {
    patterns
        .iter()
        .map(|pattern| {
            if pattern.negated {
                format!("!{}", pattern.text)
            } else {
                pattern.text.clone()
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// What a directive means to this importer.
enum Directive {
    HostName,
    User,
    Port,
    IdentityFile,
    Proxy,
    Forwarding,
    /// ForwardX11 and ForwardX11Trusted (R11).
    X11,
    /// ServerAliveInterval and ServerAliveCountMax (R20).
    Keepalive,
    Include,
    Ignored,
}

/// Classifies one directive by keyword.
///
/// A proxy or a forwarding is read into the preview. Include means files were not
/// read. Everything else is a directive this build simply does not apply.
fn classify(keyword: &str) -> Directive {
    match keyword {
        "hostname" => Directive::HostName,
        "user" => Directive::User,
        "port" => Directive::Port,
        "identityfile" => Directive::IdentityFile,
        "proxyjump" | "proxycommand" => Directive::Proxy,
        "localforward" | "remoteforward" | "dynamicforward" => Directive::Forwarding,
        "forwardx11" | "forwardx11trusted" => Directive::X11,
        "serveraliveinterval" | "serveralivecountmax" => Directive::Keepalive,
        "include" => Directive::Include,
        _ => Directive::Ignored,
    }
}

/// The first OpenSSH token in a value, when there is one.
///
/// A token is expanded by ssh from facts this importer does not have (the local user,
/// the port, the original host name), so a value containing one is reported rather
/// than applied with the token left in it. A doubled percent sign is a literal
/// percent and is not a token.
fn unsupported_token(value: &str) -> Option<String> {
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            continue;
        }
        match characters.next() {
            Some('%') => continue,
            Some(next) => return Some(format!("%{next}")),
            None => return Some("%".to_owned()),
        }
    }
    None
}

/// Builds one host from the blocks that cover its alias.
fn resolve_host(blocks: &[Block], alias: &str, alias_line: usize) -> ImportedHost {
    let mut host = ImportedHost {
        alias: alias.to_owned(),
        address: None,
        port: None,
        user: None,
        identity_file: None,
        jumps: Vec::new(),
        jump_set: false,
        jump_line: 0,
        proxy_command: None,
        proxy_set: false,
        forwards: Vec::new(),
        x11: None,
        forward_x11: None,
        forward_x11_trusted: None,
        x11_line: 0,
        alive_interval: None,
        alive_interval_line: 0,
        alive_count: None,
        alive_count_line: 0,
        withhold: false,
        outcome: ImportOutcome::Ready,
        notes: Vec::new(),
    };
    let mut blocked = false;
    let mut review = false;
    let mut hostname_seen = false;
    let mut hostname_line = 0;
    let mut reported: HashSet<(NoteKind, String)> = HashSet::new();

    for block in blocks {
        if block.matches_conditions {
            // The conditions decide whether this block applies to the alias, and this
            // importer does not evaluate them, so its settings stay unknown.
            review = true;
            let mut unknown: Vec<&str> = Vec::new();
            for setting in &block.settings {
                match classify(&setting.lower) {
                    Directive::Proxy | Directive::Forwarding => {
                        blocked = true;
                        host.withhold = true;
                        let kind = if matches!(classify(&setting.lower), Directive::Proxy) {
                            NoteKind::Proxy
                        } else {
                            NoteKind::Forwarding
                        };
                        remember_network(&mut host, setting, true);
                        push_once(
                            &mut host.notes,
                            &mut reported,
                            ImportNote::new(
                                setting.line,
                                &setting.keyword,
                                kind,
                                format!(
                                    "line {}: {} sits inside a Match block that Shellune does not evaluate, so it was not applied and this host was not imported",
                                    setting.line, setting.keyword
                                ),
                            ),
                        );
                    }
                    Directive::Include | Directive::Ignored => {}
                    // Whether a Match block applies is not evaluated, so its X11
                    // preference is unknown: applying it could turn forwarding on
                    // behind the user's back.
                    Directive::X11 | Directive::Keepalive => unknown.push(&setting.keyword),
                    Directive::HostName
                    | Directive::User
                    | Directive::Port
                    | Directive::IdentityFile => unknown.push(&setting.keyword),
                }
            }
            if !unknown.is_empty() {
                review = true;
                push_once(
                    &mut host.notes,
                    &mut reported,
                    ImportNote::new(
                        block.line,
                        "Match",
                        NoteKind::Match,
                        format!(
                            "line {}: a Match block was not evaluated, so any value it sets for {} is unknown",
                            block.line,
                            unknown.join(", ")
                        ),
                    ),
                );
            }
            continue;
        }

        if !block.covers(alias) {
            continue;
        }

        for setting in &block.settings {
            match classify(&setting.lower) {
                Directive::HostName => {
                    hostname_seen = true;
                    hostname_line = setting.line;
                    apply_text(
                        &mut host,
                        Field::Address,
                        setting,
                        &mut review,
                        &mut reported,
                    );
                }
                Directive::User => {
                    apply_text(&mut host, Field::User, setting, &mut review, &mut reported);
                }
                Directive::IdentityFile => {
                    apply_text(
                        &mut host,
                        Field::IdentityFile,
                        setting,
                        &mut review,
                        &mut reported,
                    );
                }
                Directive::Port => {
                    apply_port(&mut host, setting, &mut review, &mut reported);
                }
                Directive::Proxy => {
                    apply_proxy(&mut host, setting, &mut review, &mut reported);
                }
                Directive::Forwarding => {
                    apply_forward(&mut host, setting, &mut review, &mut reported);
                }
                Directive::X11 => {
                    apply_x11(&mut host, setting, &mut review, &mut reported);
                }
                Directive::Keepalive => {
                    apply_alive(&mut host, setting, &mut review, &mut reported);
                }
                Directive::Include => {
                    review = true;
                    push_once(
                        &mut host.notes,
                        &mut reported,
                        ImportNote::new(
                            setting.line,
                            "Include",
                            NoteKind::Include,
                            format!(
                                "line {}: Include pulls in other files that Shellune does not read, so a proxy or a jump setting from them is missing here",
                                setting.line
                            ),
                        ),
                    );
                }
                Directive::Ignored => {
                    review = true;
                    push_once(
                        &mut host.notes,
                        &mut reported,
                        ImportNote::new(
                            setting.line,
                            &setting.keyword,
                            NoteKind::Unsupported,
                            format!(
                                "line {}: {} is not applied by Shellune",
                                setting.line, setting.keyword
                            ),
                        ),
                    );
                }
            }
        }
    }

    // A host that never sets HostName is reached under its alias, which is what ssh
    // does; a HostName that is present but unusable stays unresolved, because
    // falling back to the alias would silently connect somewhere else.
    if host.address.is_none() && !hostname_seen {
        host.address = Some(alias.to_owned());
    }
    if hostname_seen && host.address.is_none() {
        push_once(
            &mut host.notes,
            &mut reported,
            ImportNote::new(
                hostname_line,
                "HostName",
                NoteKind::InvalidValue,
                "the HostName on this line cannot be used, so no address was taken from the file",
            ),
        );
    }
    host.x11 = settle_x11(&host);
    if host.user.is_none() {
        review = true;
        host.notes.push(ImportNote::new(
            alias_line,
            "User",
            NoteKind::MissingField,
            "no User applies to this host, and Shellune needs an explicit user name to save it"
                .to_owned(),
        ));
    }
    if host.jump_set && !host.jumps.is_empty() && host.proxy_command.is_some() {
        blocked = true;
        host.withhold = true;
        host.notes.push(ImportNote::new(
            alias_line,
            "ProxyJump",
            NoteKind::Proxy,
            format!(
                "{alias} has both a jump chain and a ProxyCommand. Keep one way to reach it; neither was applied, and the host was not imported"
            ),
        ));
    }

    host.outcome = if blocked || host.withhold {
        ImportOutcome::Blocked
    } else if review {
        ImportOutcome::NeedsReview
    } else {
        ImportOutcome::Ready
    };
    host
}

/// The four settings this importer applies to a record.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Address,
    User,
    IdentityFile,
}

/// Applies one text setting, keeping the first obtained value.
fn apply_text(
    host: &mut ImportedHost,
    field: Field,
    setting: &Setting,
    review: &mut bool,
    reported: &mut HashSet<(NoteKind, String)>,
) {
    let slot = match field {
        Field::Address => &mut host.address,
        Field::User => &mut host.user,
        Field::IdentityFile => &mut host.identity_file,
    };
    if let Some(existing) = slot.as_deref() {
        if existing != setting.value {
            push_once(
                &mut host.notes,
                reported,
                ImportNote::new(
                    setting.line,
                    &setting.keyword,
                    NoteKind::Superseded,
                    format!(
                        "line {}: {} {} was ignored because an earlier line already set it to {}",
                        setting.line, setting.keyword, setting.value, existing
                    ),
                ),
            );
        }
        return;
    }

    if let Some(token) = unsupported_token(&setting.value) {
        *review = true;
        push_once(
            &mut host.notes,
            reported,
            ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::Token,
                format!(
                    "line {}: {} uses the OpenSSH token {}, which Shellune does not expand",
                    setting.line, setting.keyword, token
                ),
            ),
        );
        return;
    }

    let usable = match field {
        Field::Address => !setting.value.is_empty() && !setting.value.contains(char::is_whitespace),
        Field::User => !setting.value.is_empty() && !setting.value.contains(char::is_whitespace),
        Field::IdentityFile => !setting.value.is_empty(),
    };
    if !usable {
        *review = true;
        push_once(
            &mut host.notes,
            reported,
            ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::InvalidValue,
                format!(
                    "line {}: {} {} is not a value Shellune can use",
                    setting.line, setting.keyword, setting.value
                ),
            ),
        );
        return;
    }

    *slot = Some(setting.value.clone());
}

/// Applies one Port setting, keeping the first obtained value.
fn apply_port(
    host: &mut ImportedHost,
    setting: &Setting,
    review: &mut bool,
    reported: &mut HashSet<(NoteKind, String)>,
) {
    if let Some(existing) = host.port {
        let parsed = setting.value.parse::<u16>().ok();
        if parsed != Some(existing) {
            push_once(
                &mut host.notes,
                reported,
                ImportNote::new(
                    setting.line,
                    &setting.keyword,
                    NoteKind::Superseded,
                    format!(
                        "line {}: Port {} was ignored because an earlier line already set the port to {existing}",
                        setting.line, setting.value
                    ),
                ),
            );
        }
        return;
    }

    match setting.value.parse::<u16>() {
        Ok(port) if port > 0 => host.port = Some(port),
        _ => {
            *review = true;
            push_once(
                &mut host.notes,
                reported,
                ImportNote::new(
                    setting.line,
                    &setting.keyword,
                    NoteKind::InvalidValue,
                    format!(
                        "line {}: Port {} is outside 1-65535, so the SSH default 22 is used",
                        setting.line, setting.value
                    ),
                ),
            );
        }
    }
}

/// Applies one `ForwardX11` or `ForwardX11Trusted`, keeping the first obtained value.
///
/// Both take `yes` or `no`, and the two are read separately: a later
/// `ForwardX11Trusted yes` does not rewrite the `ForwardX11` line that came before it.
/// What the pair adds up to is decided by [`settle_x11`] once every block is read.
fn apply_x11(
    host: &mut ImportedHost,
    setting: &Setting,
    review: &mut bool,
    reported: &mut HashSet<(NoteKind, String)>,
) {
    let parsed = match setting.value.trim().to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" => Some(true),
        "no" | "false" | "0" => Some(false),
        _ => None,
    };
    let Some(value) = parsed else {
        *review = true;
        push_once(
            &mut host.notes,
            reported,
            ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::InvalidValue,
                format!(
                    "line {}: {} takes yes or no, so \"{}\" was not applied and the X11 mode is unknown",
                    setting.line, setting.keyword, setting.value
                ),
            ),
        );
        return;
    };
    let trusted = setting.lower == "forwardx11trusted";
    let slot = if trusted {
        &mut host.forward_x11_trusted
    } else {
        &mut host.forward_x11
    };
    if let Some(existing) = *slot {
        if existing != value {
            push_once(
                &mut host.notes,
                reported,
                ImportNote::new(
                    setting.line,
                    &setting.keyword,
                    NoteKind::Superseded,
                    format!(
                        "line {}: {} {} was ignored because an earlier line already set it to {}",
                        setting.line,
                        setting.keyword,
                        setting.value,
                        if existing { "yes" } else { "no" }
                    ),
                ),
            );
        }
        return;
    }
    *slot = Some(value);
    host.x11_line = setting.line;
}

/// What a host's `ForwardX11` and `ForwardX11Trusted` lines add up to.
///
/// The result is the three-value mode OpenSSH names on its command line: a trusted
/// `yes` is `-Y` (and enables forwarding on its own, as ssh does), a plain `ForwardX11
/// yes` is `-X`, and anything else that was set is off. A file that sets neither says
/// nothing, which is the off default and not a claim about the file.
fn settle_x11(host: &ImportedHost) -> Option<X11Forwarding> {
    match (host.forward_x11, host.forward_x11_trusted) {
        (None, None) => None,
        (_, Some(true)) => Some(X11Forwarding::Trusted),
        (Some(true), _) => Some(X11Forwarding::Restricted),
        _ => Some(X11Forwarding::Off),
    }
}

/// Applies one `ServerAliveInterval` or `ServerAliveCountMax` (R20).
///
/// The two are independent, exactly as OpenSSH defines them: a file that sets only the
/// count keeps whatever interval applies, which is why the importer stores the two
/// numbers separately instead of inventing a pair. First obtained value wins, like the
/// other scalar directives.
///
/// Interval `0` is OpenSSH's spelling of "do not probe" and is kept as an explicit
/// disable. A count of `0` is not stored: the SSH library reads a maximum of zero as
/// "never treat this connection as dead", which is the opposite of what the user would
/// be choosing, so the line is refused with that reason and the host is shown for review.
fn apply_alive(
    host: &mut ImportedHost,
    setting: &Setting,
    review: &mut bool,
    reported: &mut HashSet<(NoteKind, String)>,
) {
    let is_interval = setting.lower == "serveraliveinterval";
    let existing = if is_interval {
        host.alive_interval
    } else {
        host.alive_count
    };
    if let Some(existing) = existing {
        if setting.value.parse::<u32>().ok() != Some(existing) {
            push_once(
                &mut host.notes,
                reported,
                ImportNote::new(
                    setting.line,
                    &setting.keyword,
                    NoteKind::Superseded,
                    format!(
                        "line {}: {} {} was ignored because an earlier line already set it to {existing}",
                        setting.line, setting.keyword, setting.value
                    ),
                ),
            );
        }
        return;
    }

    let Ok(value) = setting.value.parse::<u32>() else {
        *review = true;
        push_once(
            &mut host.notes,
            reported,
            ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::InvalidValue,
                format!(
                    "line {}: {} {} is not a whole number, so no keepalive value was taken from it",
                    setting.line, setting.keyword, setting.value
                ),
            ),
        );
        return;
    };

    // The bounds are the record's own, so a value the preview accepts is a value the
    // saved host can hold; the reason shown here is the reason a form would show.
    let checked = if is_interval {
        HostConnection::new(Some(value), None, None)
    } else {
        HostConnection::new(None, Some(value), None)
    };
    if let Err(error) = checked {
        *review = true;
        push_once(
            &mut host.notes,
            reported,
            ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::InvalidValue,
                format!(
                    "line {}: {} {} was not applied: {error}",
                    setting.line, setting.keyword, setting.value
                ),
            ),
        );
        return;
    }

    if is_interval {
        host.alive_interval = Some(value);
        host.alive_interval_line = setting.line;
    } else {
        host.alive_count = Some(value);
        host.alive_count_line = setting.line;
    }
}

/// How a preview row describes the keepalive the file asked for (R20).
fn describe_connection(connection: HostConnection) -> String {
    match (
        connection.keepalive_interval_secs(),
        connection.keepalive_max_misses(),
    ) {
        (Some(0), _) => "keepalive off".to_owned(),
        (Some(interval), Some(misses)) => format!("keepalive {interval}s x {misses}"),
        (Some(interval), None) => format!("keepalive {interval}s"),
        (None, Some(misses)) => format!("keepalive after {misses} unanswered"),
        (None, None) => "keepalive inherited".to_owned(),
    }
}

/// Applies one ProxyJump or ProxyCommand, keeping the first obtained value.
fn apply_proxy(
    host: &mut ImportedHost,
    setting: &Setting,
    review: &mut bool,
    reported: &mut HashSet<(NoteKind, String)>,
) {
    let is_jump = setting.lower == "proxyjump";
    let already = if is_jump {
        host.jump_set
    } else {
        host.proxy_set
    };
    if already {
        push_once(
            &mut host.notes,
            reported,
            ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::Superseded,
                format!(
                    "line {}: {} {} was ignored because an earlier line already set it",
                    setting.line, setting.keyword, setting.value
                ),
            ),
        );
        return;
    }
    if is_jump {
        host.jump_set = true;
        host.jump_line = setting.line;
    } else {
        host.proxy_set = true;
    }
    if setting.value.eq_ignore_ascii_case("none") {
        return;
    }
    if let Some(token) = proxy_token(&setting.value) {
        host.withhold = true;
        *review = true;
        push_once(
            &mut host.notes,
            reported,
            ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::Token,
                format!(
                    "line {}: {} uses the OpenSSH token {token}, which Shellune does not expand for this directive, so the host was not imported",
                    setting.line, setting.keyword
                ),
            ),
        );
        remember_network(host, setting, true);
        return;
    }
    if is_jump {
        match parse_jump_list(&setting.value) {
            Ok(jumps) => host.jumps = jumps,
            Err(reason) => {
                host.withhold = true;
                *review = true;
                host.notes.push(ImportNote::new(
                    setting.line,
                    &setting.keyword,
                    NoteKind::InvalidValue,
                    format!(
                        "line {}: ProxyJump {} cannot be stored ({reason}), so the host was not imported as a direct connection",
                        setting.line, setting.value
                    ),
                ));
            }
        }
    } else if setting.value.is_empty() {
        host.withhold = true;
        *review = true;
        host.notes.push(ImportNote::new(
            setting.line,
            &setting.keyword,
            NoteKind::InvalidValue,
            format!(
                "line {}: ProxyCommand has no command, so the host was not imported",
                setting.line
            ),
        ));
    } else {
        host.proxy_command = Some(setting.value.clone());
    }
}

/// Applies one forwarding directive. Every complete line is kept; OpenSSH accumulates them.
fn apply_forward(
    host: &mut ImportedHost,
    setting: &Setting,
    review: &mut bool,
    _reported: &mut HashSet<(NoteKind, String)>,
) {
    let kind = match setting.lower.as_str() {
        "localforward" => ForwardKind::Local,
        "remoteforward" => ForwardKind::Remote,
        _ => ForwardKind::Dynamic,
    };
    if let Some(token) = proxy_token(&setting.value) {
        *review = true;
        host.notes.push(ImportNote::new(
            setting.line,
            &setting.keyword,
            NoteKind::Token,
            format!(
                "line {}: {} uses the OpenSSH token {token}, which Shellune does not expand, so this forward was not created",
                setting.line, setting.keyword
            ),
        ));
        return;
    }
    match parse_forward(kind, &setting.value, setting.line) {
        Ok(forward) => host.forwards.push(forward),
        Err(reason) => {
            *review = true;
            host.notes.push(ImportNote::new(
                setting.line,
                &setting.keyword,
                NoteKind::InvalidValue,
                format!(
                    "line {}: {} {} is incomplete ({reason}) and was not created",
                    setting.line, setting.keyword, setting.value
                ),
            ));
        }
    }
}

/// Keeps the text of a proxy that will not be saved, so the preview can still show it.
fn remember_network(host: &mut ImportedHost, setting: &Setting, withheld: bool) {
    if !withheld {
        return;
    }
    if setting.lower == "proxycommand" && !setting.value.eq_ignore_ascii_case("none") {
        host.proxy_command = Some(setting.value.clone());
    }
    if setting.lower == "proxyjump"
        && host.jumps.is_empty()
        && let Ok(jumps) = parse_jump_list(&setting.value)
    {
        host.jumps = jumps;
    }
}

/// Tokens the connection layer expands in a ProxyCommand: `%h`, `%n`, `%p`, `%r`, `%%`.
///
/// Any other percent token is reported. HostName and User still reject every token,
/// because those values are stored literally and a token there would be dialled as text.
fn proxy_token(value: &str) -> Option<String> {
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            continue;
        }
        match characters.next() {
            Some('%') => continue,
            Some('h' | 'n' | 'p' | 'r') => continue,
            Some(next) => return Some(format!("%{next}")),
            None => return Some("%".to_owned()),
        }
    }
    None
}

fn parse_jump_list(value: &str) -> Result<Vec<JumpSpec>, String> {
    let mut hops = Vec::new();
    for part in value.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("a hop is missing".to_owned());
        }
        if part.chars().any(char::is_whitespace) {
            return Err("a hop contains whitespace".to_owned());
        }
        hops.push(parse_jump_hop(part)?);
    }
    if hops.is_empty() {
        return Err("no hop was named".to_owned());
    }
    Ok(hops)
}

fn parse_jump_hop(token: &str) -> Result<JumpSpec, String> {
    let (user, rest) = if let Some((user, rest)) = token.split_once('@') {
        if user.is_empty() || rest.is_empty() {
            return Err("the user or the host is missing".to_owned());
        }
        (Some(user.to_owned()), rest)
    } else {
        (None, token)
    };
    let (address, port) = if let Some(rest) = rest.strip_prefix('[') {
        let Some((address, after)) = rest.split_once(']') else {
            return Err("the IPv6 address is not closed".to_owned());
        };
        if address.is_empty() {
            return Err("the address is empty".to_owned());
        }
        let port = if after.is_empty() {
            None
        } else if let Some(port) = after.strip_prefix(':') {
            Some(parse_port_token(port)?)
        } else {
            return Err("trailing text after the address".to_owned());
        };
        (address.to_owned(), port)
    } else if let Some((address, port)) = rest.rsplit_once(':') {
        if !port.is_empty() && port.chars().all(|character| character.is_ascii_digit()) {
            (address.to_owned(), Some(parse_port_token(port)?))
        } else {
            (rest.to_owned(), None)
        }
    } else {
        (rest.to_owned(), None)
    };
    if address.is_empty() {
        return Err("the address is empty".to_owned());
    }
    Ok(JumpSpec {
        alias: None,
        user,
        address,
        port,
    })
}

fn parse_port_token(text: &str) -> Result<u16, String> {
    match text.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(format!("port {text} is outside 1-65535")),
    }
}

fn parse_forward(kind: ForwardKind, value: &str, line: usize) -> Result<ImportedForward, String> {
    if value.chars().any(|character| character == '/') {
        return Err("a Unix socket path is not a TCP forward".to_owned());
    }
    let parts: Vec<&str> = value.split_whitespace().collect();
    match kind {
        ForwardKind::Dynamic => {
            if parts.len() != 1 {
                return Err("DynamicForward needs one listen port".to_owned());
            }
            let (listen_host, listen_port) = parse_listen(parts[0])?;
            Ok(ImportedForward {
                kind,
                listen_host,
                listen_port,
                target_host: None,
                target_port: None,
                line,
            })
        }
        ForwardKind::Local | ForwardKind::Remote => {
            if parts.len() != 2 {
                return Err("the line needs a listen port and a target".to_owned());
            }
            let (listen_host, listen_port) = parse_listen(parts[0])?;
            let (target_host, target_port) = parse_target(parts[1])?;
            Ok(ImportedForward {
                kind,
                listen_host,
                listen_port,
                target_host: Some(target_host),
                target_port: Some(target_port),
                line,
            })
        }
    }
}

fn parse_listen(token: &str) -> Result<(String, u16), String> {
    if token.contains('[') || token.contains(']') {
        let (address, port) = parse_bracket_host(token)?;
        let port = port.ok_or_else(|| "the listen port is missing".to_owned())?;
        return Ok((address, port));
    }
    if let Some((host, port)) = token.rsplit_once(':')
        && !port.is_empty()
        && port.chars().all(|character| character.is_ascii_digit())
        && !host.is_empty()
    {
        return Ok((host.to_owned(), parse_port_token(port)?));
    }
    Ok(("127.0.0.1".to_owned(), parse_port_token(token)?))
}

fn parse_target(token: &str) -> Result<(String, u16), String> {
    if token.contains('[') {
        let (address, port) = parse_bracket_host(token)?;
        let port = port.ok_or_else(|| "the target port is missing".to_owned())?;
        return Ok((address, port));
    }
    let Some((host, port)) = token.rsplit_once(':') else {
        return Err("the target needs a host and a port".to_owned());
    };
    if host.is_empty() {
        return Err("the target host is empty".to_owned());
    }
    Ok((host.to_owned(), parse_port_token(port)?))
}

fn parse_bracket_host(token: &str) -> Result<(String, Option<u16>), String> {
    let Some(rest) = token.strip_prefix('[') else {
        return Err("the address is not bracketed".to_owned());
    };
    let Some((address, after)) = rest.split_once(']') else {
        return Err("the IPv6 address is not closed".to_owned());
    };
    if address.is_empty() {
        return Err("the address is empty".to_owned());
    }
    if after.is_empty() {
        return Ok((address.to_owned(), None));
    }
    let Some(port) = after.strip_prefix(':') else {
        return Err("trailing text after the address".to_owned());
    };
    Ok((address.to_owned(), Some(parse_port_token(port)?)))
}

/// Binds ProxyJump hops to aliases in this file, and withholds a host whose hop
/// cannot be stored without inventing a user or a direct connection.
fn finalize_jumps(hosts: &mut [ImportedHost]) {
    let snapshots: Vec<(String, Option<String>, u16, Option<String>)> = hosts
        .iter()
        .map(|host| {
            (
                host.alias.clone(),
                host.user.clone(),
                host.port(),
                host.address.clone(),
            )
        })
        .collect();
    for host in hosts.iter_mut() {
        if host.jumps.is_empty() || host.withhold {
            continue;
        }
        let mut resolved = Vec::new();
        let mut failure: Option<String> = None;
        for jump in host.jumps.clone() {
            if let Some((alias, user, port, address)) = snapshots
                .iter()
                .find(|(alias, _, _, _)| alias.eq_ignore_ascii_case(&jump.address))
            {
                let user_ok = match &jump.user {
                    Some(asked) => user.as_ref() == Some(asked),
                    None => true,
                };
                let port_ok = match jump.port {
                    Some(asked) => asked == *port,
                    None => true,
                };
                if user.is_none() || !user_ok || !port_ok {
                    failure = Some(format!(
                        "ProxyJump {} does not match the saved user and port of {alias}, so it was not applied",
                        jump.label()
                    ));
                    break;
                }
                resolved.push(JumpSpec {
                    alias: Some(alias.clone()),
                    user: user.clone(),
                    address: address.clone().unwrap_or_else(|| alias.clone()),
                    port: Some(*port),
                });
            } else if jump.user.is_some() {
                resolved.push(jump);
            } else {
                failure = Some(format!(
                    "ProxyJump {} names a host with no user in this file, so it was not applied and this host was not imported as a direct connection",
                    jump.label()
                ));
                break;
            }
        }
        if let Some(reason) = failure {
            let line = host.jump_line.max(1);
            host.withhold = true;
            host.outcome = ImportOutcome::Blocked;
            host.notes
                .push(ImportNote::new(line, "ProxyJump", NoteKind::Proxy, reason));
        } else {
            host.jumps = resolved;
        }
    }
    // A hop that points at a host this preview will not save cannot be linked.
    loop {
        let saved: Vec<String> = hosts
            .iter()
            .filter(|host| host.is_importable())
            .map(|host| host.alias.to_lowercase())
            .collect();
        let mut changed = false;
        for host in hosts.iter_mut() {
            if !host.is_importable() {
                continue;
            }
            let missing = host.jumps.iter().any(|jump| {
                jump.alias().is_some_and(|alias| {
                    !saved.iter().any(|saved| saved.eq_ignore_ascii_case(alias))
                })
            });
            if missing {
                let line = host.jump_line.max(1);
                host.withhold = true;
                host.outcome = ImportOutcome::Blocked;
                host.notes.push(ImportNote::new(
                    line,
                    "ProxyJump",
                    NoteKind::Proxy,
                    format!(
                        "the jump chain for {} depends on a host that is not imported, so {} was not imported as a direct connection",
                        host.alias, host.alias
                    ),
                ));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
}

/// Records a note once per kind and keyword, so one block cannot flood a row.
fn push_once(
    notes: &mut Vec<ImportNote>,
    reported: &mut HashSet<(NoteKind, String)>,
    note: ImportNote,
) {
    if reported.insert((note.kind, note.keyword.clone())) {
        notes.push(note);
    }
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// The home directory of the current user, as the platform reports it.
#[must_use]
pub fn home_directory() -> Option<PathBuf> {
    if let Some(home) = non_empty_env("HOME") {
        return Some(PathBuf::from(home));
    }
    if let Some(home) = non_empty_env("USERPROFILE") {
        return Some(PathBuf::from(home));
    }
    match (non_empty_env("HOMEDRIVE"), non_empty_env("HOMEPATH")) {
        (Some(drive), Some(path)) => Some(PathBuf::from(format!("{drive}{path}"))),
        _ => None,
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var_os(name)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.trim().is_empty())
}

/// Resolves a leading tilde against the home directory.
///
/// Nothing else is expanded: a relative path stays relative to the process, so a
/// mistyped path fails by naming the file it really looked for.
#[must_use]
pub fn resolve_path(text: &str) -> PathBuf {
    let text = text.trim();
    if text == "~" {
        return home_directory().unwrap_or_else(|| PathBuf::from(text));
    }
    for prefix in ["~/", "~\\"] {
        if let Some(rest) = text.strip_prefix(prefix)
            && let Some(home) = home_directory()
        {
            return home.join(rest);
        }
    }
    PathBuf::from(text)
}

/// The configuration file ssh reads when no other file is named.
#[must_use]
pub fn default_config_path() -> Option<PathBuf> {
    home_directory().map(|home| home.join(".ssh").join("config"))
}

// ---------------------------------------------------------------------------
// Wildcards
// ---------------------------------------------------------------------------

/// Matches one Host pattern against an alias, the way ssh compares them.
///
/// The syntax is the one ssh uses for Host lines: * matches any run of characters,
/// ? matches exactly one, and everything else matches itself. The comparison is
/// case-insensitive because host names are.
fn glob_match(pattern: &str, alias: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let alias: Vec<char> = alias.chars().flat_map(char::to_lowercase).collect();
    let (mut pattern_at, mut alias_at) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut resume = 0usize;

    while alias_at < alias.len() {
        if pattern_at < pattern.len()
            && (pattern[pattern_at] == '?' || pattern[pattern_at] == alias[alias_at])
        {
            pattern_at += 1;
            alias_at += 1;
        } else if pattern_at < pattern.len() && pattern[pattern_at] == '*' {
            star = Some(pattern_at);
            resume = alias_at;
            pattern_at += 1;
        } else if let Some(star_at) = star {
            pattern_at = star_at + 1;
            resume += 1;
            alias_at = resume;
        } else {
            return false;
        }
    }
    while pattern_at < pattern.len() && pattern[pattern_at] == '*' {
        pattern_at += 1;
    }
    pattern_at == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A preview of configuration text, for the tests that expect a preview rather
    /// than an error.
    fn parse_preview(text: &str) -> ImportPreview {
        ImportPreview::from_text("C:/Users/example/.ssh/config", text).expect("the file parses")
    }

    fn host<'a>(preview: &'a ImportPreview, alias: &str) -> &'a ImportedHost {
        preview
            .hosts()
            .iter()
            .find(|host| host.alias() == alias)
            .unwrap_or_else(|| panic!("the file describes {alias}"))
    }

    const SAMPLE: &str = "\
# A comment, which is not a statement
Host web
    HostName web.example.org
    User deploy
    Port 2222
    IdentityFile ~/.ssh/id_ed25519

host db
  hostname db.internal
  user postgres
  port=5432
";

    #[test]
    fn the_supported_subset_is_read_with_the_fields_a_connection_needs() {
        let preview = parse_preview(SAMPLE);
        assert_eq!(preview.source(), Path::new("C:/Users/example/.ssh/config"));
        assert_eq!(preview.hosts().len(), 2, "two concrete aliases");

        let web = host(&preview, "web");
        assert_eq!(web.address(), Some("web.example.org"));
        assert_eq!(web.user(), Some("deploy"));
        assert_eq!(web.port(), 2222);
        assert_eq!(web.identity_file(), Some("~/.ssh/id_ed25519"));
        assert_eq!(web.outcome(), ImportOutcome::Ready);
        assert!(web.notes().is_empty(), "{:?}", web.notes());
        assert_eq!(web.target(), "deploy@web.example.org:2222");
        assert_eq!(
            web.summary(),
            "deploy@web.example.org:2222 · ~/.ssh/id_ed25519"
        );
        assert!(web.is_importable());

        // Keywords are case-insensitive, and Port=value is the same statement as
        // "Port value".
        let db = host(&preview, "db");
        assert_eq!(db.address(), Some("db.internal"));
        assert_eq!(db.user(), Some("postgres"));
        assert_eq!(db.port(), 5432);
        assert_eq!(db.identity_file(), None);
        assert!(preview.limits().ignored().is_empty());
        assert!(
            !preview.is_partial(),
            "nothing was skipped, so nothing is partial"
        );
    }

    #[test]
    fn x11_forwarding_is_read_from_the_two_directives_and_stays_off_by_default() {
        let preview = parse_preview(
            "\
Host restricted
    HostName r.example.org
    User deploy
    ForwardX11 yes

Host trusted
    HostName t.example.org
    User deploy
    ForwardX11Trusted yes

Host off
    HostName o.example.org
    User deploy
    ForwardX11 no

Host silent
    HostName s.example.org
    User deploy
",
        );
        assert_eq!(
            host(&preview, "restricted").x11(),
            Some(X11Forwarding::Restricted),
            "ForwardX11 yes is the restricted mode of ssh -X"
        );
        assert_eq!(
            host(&preview, "trusted").x11(),
            Some(X11Forwarding::Trusted),
            "ForwardX11Trusted yes enables forwarding and lifts the security limits"
        );
        assert_eq!(host(&preview, "off").x11(), Some(X11Forwarding::Off));
        assert_eq!(
            host(&preview, "silent").x11(),
            None,
            "a file that says nothing about X11 does not turn it on"
        );
        assert!(
            !host(&preview, "silent").summary().contains("X11"),
            "an X11 mode that was not set is not in the summary"
        );
        assert!(
            host(&preview, "trusted").summary().contains("X11 trusted"),
            "{}",
            host(&preview, "trusted").summary()
        );
    }

    #[test]
    fn the_plan_carries_the_x11_mode_and_a_later_line_does_not_override_the_first() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    ForwardX11 no
    ForwardX11 yes
    ForwardX11Trusted no
",
        );
        let web = host(&preview, "web");
        assert_eq!(
            web.x11(),
            Some(X11Forwarding::Off),
            "the first ForwardX11 line is the one that counts"
        );
        assert!(
            web.notes()
                .iter()
                .any(|note| note.kind() == NoteKind::Superseded && note.keyword() == "ForwardX11"),
            "a superseded X11 line is reported: {:?}",
            web.notes()
        );

        let book = HostBook::default();
        let plan = preview.plan(&book);
        let record = plan
            .edits()
            .iter()
            .find(|edit| edit.host().name() == "web")
            .expect("the host is planned")
            .host();
        assert_eq!(record.x11(), X11Forwarding::Off);
    }

    #[test]
    fn a_value_forwardx11_cannot_use_is_reported_and_does_not_turn_forwarding_on() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    ForwardX11 maybe
",
        );
        let web = host(&preview, "web");
        assert_eq!(
            web.x11(),
            None,
            "a value that was not applied sets no mode, so forwarding stays off"
        );
        assert_eq!(
            web.outcome(),
            ImportOutcome::NeedsReview,
            "a value that was not applied needs a look"
        );
        assert!(
            web.notes().iter().any(|note| {
                note.kind() == NoteKind::InvalidValue && note.keyword() == "ForwardX11"
            }),
            "{:?}",
            web.notes()
        );
    }

    #[test]
    fn an_x11_line_inside_a_match_block_is_not_applied_behind_the_users_back() {
        let preview = parse_preview(
            "\
Match host web
    ForwardX11Trusted yes

Host web
    HostName web.example.org
    User deploy
",
        );
        let web = host(&preview, "web");
        assert_eq!(
            web.x11(),
            None,
            "an X11 mode from a Match block this importer does not evaluate is unknown"
        );
        assert!(
            web.notes()
                .iter()
                .any(|note| note.kind() == NoteKind::Match),
            "{:?}",
            web.notes()
        );
    }

    #[test]
    fn the_first_obtained_value_wins_and_a_later_one_is_reported() {
        let preview = parse_preview(
            "\
Host web
    HostName first.example.org
    HostName second.example.org
    User deploy
    Port 22
    Port 2222

Host web
    HostName third.example.org
    User ops
",
        );
        let web = host(&preview, "web");
        assert_eq!(web.address(), Some("first.example.org"));
        assert_eq!(web.user(), Some("deploy"));
        assert_eq!(web.port(), 22);

        let superseded: Vec<&ImportNote> = web
            .notes()
            .iter()
            .filter(|note| note.kind() == NoteKind::Superseded)
            .collect();
        assert_eq!(superseded.len(), 3, "{:?}", web.notes());
        assert!(
            superseded
                .iter()
                .any(|note| note.keyword() == "HostName"
                    && note.reason().contains("second.example.org")),
            "the ignored value is named: {:?}",
            web.notes()
        );
        assert!(
            superseded.iter().any(|note| note.line() == 3),
            "a note points at the line that was ignored"
        );
        // A superseded line is an explanation, not a limitation.
        assert_eq!(web.outcome(), ImportOutcome::Ready);
    }

    #[test]
    fn a_missing_hostname_means_the_alias_and_a_missing_user_stops_the_import() {
        let preview = parse_preview(
            "\
Host lab
    Port 2222
",
        );
        let lab = host(&preview, "lab");
        assert_eq!(
            lab.address(),
            Some("lab"),
            "ssh reaches a host under its alias"
        );
        assert_eq!(lab.port(), 2222);
        assert_eq!(lab.user(), None);
        assert_eq!(lab.outcome(), ImportOutcome::NeedsReview);
        assert!(
            !lab.is_importable(),
            "a record needs an explicit user, so nothing is invented"
        );
        let missing = lab
            .notes()
            .iter()
            .find(|note| note.kind() == NoteKind::MissingField)
            .expect("the missing user is explained");
        assert_eq!(missing.keyword(), "User");
        assert!(missing.reason().contains("user"));
        assert_eq!(preview.importable_count(), 0);
    }

    #[test]
    fn directives_before_the_first_host_line_apply_to_every_host() {
        let preview = parse_preview(
            "\
User shared
Port 2200

Host web
    HostName web.example.org
",
        );
        let web = host(&preview, "web");
        assert_eq!(web.user(), Some("shared"));
        assert_eq!(web.port(), 2200);
        assert_eq!(web.outcome(), ImportOutcome::Ready);
    }

    #[test]
    fn a_wildcard_block_reaches_only_the_aliases_it_matches() {
        let matched = parse_preview(
            "\
Host web* !web3
    User webops

Host web1 web2 web3 db
",
        );
        assert_eq!(matched.hosts().len(), 4, "four concrete aliases");
        assert_eq!(host(&matched, "web1").user(), Some("webops"));
        assert_eq!(host(&matched, "web2").user(), Some("webops"));
        assert_eq!(
            host(&matched, "web3").user(),
            None,
            "a negated pattern keeps the block away"
        );
        assert_eq!(host(&matched, "db").user(), None);

        assert!(glob_match("*.internal", "DB.Internal"));
        assert!(glob_match("web?", "web1"));
        assert!(!glob_match("web?", "web"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn an_unsupported_directive_is_reported_instead_of_being_applied() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    Compression yes
    HostKeyAlgorithms ssh-ed25519
",
        );
        let web = host(&preview, "web");
        assert_eq!(web.outcome(), ImportOutcome::NeedsReview);
        assert!(
            web.is_importable(),
            "a directive that is not applied does not block a connection"
        );
        assert_eq!(web.notes().len(), 2);
        assert!(
            web.notes()
                .iter()
                .all(|note| note.kind() == NoteKind::Unsupported)
        );
        assert_eq!(preview.limits().ignored().len(), 2);
        assert!(preview.limits().is_partial());
        assert!(
            preview.limits().ignored()[0]
                .reason()
                .contains("not applied")
        );
    }

    #[test]
    fn keepalive_directives_are_read_instead_of_being_ignored() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    ServerAliveInterval 45
    ServerAliveCountMax 5
",
        );
        let web = host(&preview, "web");
        assert_eq!(
            web.outcome(),
            ImportOutcome::Ready,
            "both directives are applied, so nothing is left for review: {:?}",
            web.notes()
        );
        let connection = web.connection().expect("the file sets a keepalive");
        assert_eq!(connection.keepalive_interval_secs(), Some(45));
        assert_eq!(connection.keepalive_max_misses(), Some(5));
        assert!(
            preview.limits().ignored().is_empty(),
            "an applied directive is not also listed as unsupported: {:?}",
            preview.limits().ignored()
        );
        assert!(
            web.summary().contains("keepalive 45s"),
            "the preview row states what will be applied: {}",
            web.summary()
        );

        // The two directives are independent, exactly as in ssh: a file that sets only
        // the count keeps the interval that applies globally.
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    ServerAliveCountMax 2
",
        );
        let web = host(&preview, "web");
        let connection = web.connection().expect("the count is kept");
        assert_eq!(connection.keepalive_interval_secs(), None);
        assert_eq!(connection.keepalive_max_misses(), Some(2));
        assert!(web.summary().contains("2 unanswered"), "{}", web.summary());
    }

    #[test]
    fn an_interval_of_zero_is_an_explicit_off_and_not_an_inherited_default() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    ServerAliveInterval 0
",
        );
        let web = host(&preview, "web");
        assert_eq!(
            web.outcome(),
            ImportOutcome::Ready,
            "0 is a value OpenSSH documents: {:?}",
            web.notes()
        );
        let connection = web.connection().expect("the disable is kept");
        assert_eq!(connection.keepalive_interval_secs(), Some(0));
        assert!(
            web.summary().contains("keepalive off"),
            "the preview says the file turns keepalive off: {}",
            web.summary()
        );
        assert!(
            !connection.is_empty(),
            "an explicit off is a choice, not inheritance"
        );
        let policy = shellune_core::ConnectionDefaults::default().policy_for(Some(connection));
        assert!(
            policy.keepalive().is_none(),
            "the exported host is dialled with keepalive off"
        );
    }

    #[test]
    fn an_unusable_keepalive_value_is_refused_with_its_reason() {
        let preview = parse_preview(
            "\
Host count-zero
    HostName a.example.org
    User deploy
    ServerAliveCountMax 0

Host interval-word
    HostName b.example.org
    User deploy
    ServerAliveInterval soon

Host interval-huge
    HostName c.example.org
    User deploy
    ServerAliveInterval 99999
",
        );
        let count = host(&preview, "count-zero");
        assert_eq!(count.outcome(), ImportOutcome::NeedsReview);
        assert!(count.is_importable(), "one unusable line is a review");
        assert!(count.connection().is_none(), "nothing was stored");
        let note = count
            .notes()
            .iter()
            .find(|note| note.keyword() == "ServerAliveCountMax")
            .expect("the refused line is named");
        assert_eq!(note.kind(), NoteKind::InvalidValue);
        assert_eq!(note.line(), 4);
        assert!(
            note.reason().contains("silent connection"),
            "the reason says what a count of 0 would mean: {}",
            note.reason()
        );

        let word = host(&preview, "interval-word");
        assert_eq!(word.outcome(), ImportOutcome::NeedsReview);
        let note = word
            .notes()
            .iter()
            .find(|note| note.keyword() == "ServerAliveInterval")
            .expect("the refused line is named");
        assert!(note.reason().contains("whole number"), "{}", note.reason());

        let huge = host(&preview, "interval-huge");
        assert_eq!(huge.outcome(), ImportOutcome::NeedsReview);
        assert!(huge.connection().is_none());
        let note = huge
            .notes()
            .iter()
            .find(|note| note.keyword() == "ServerAliveInterval")
            .expect("the refused line is named");
        assert!(
            note.reason().contains("above the 3600s"),
            "the reason states the boundary: {}",
            note.reason()
        );
    }

    #[test]
    fn tcp_keepalive_is_still_named_as_not_applied() {
        // TCPKeepAlive is a transport-layer probe that can be forged by a middlebox,
        // so it is not a substitute for the encrypted-channel probe. The file's line is
        // reported, not silently treated as keepalive and not silently dropped.
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    TCPKeepAlive no
    ServerAliveInterval 20
",
        );
        let web = host(&preview, "web");
        assert_eq!(web.outcome(), ImportOutcome::NeedsReview);
        assert_eq!(
            web.connection().and_then(|c| c.keepalive_interval_secs()),
            Some(20),
            "the supported directive is still applied"
        );
        let note = web
            .notes()
            .iter()
            .find(|note| note.keyword() == "TCPKeepAlive")
            .expect("TCPKeepAlive is reported");
        assert_eq!(note.kind(), NoteKind::Unsupported);
        assert!(note.reason().contains("not applied"), "{}", note.reason());
        assert!(
            preview
                .limits()
                .ignored()
                .iter()
                .any(|item| item.keyword() == "TCPKeepAlive"),
            "the file-level list names it too: {:?}",
            preview.limits().ignored()
        );
        assert!(preview.is_partial());
    }

    #[test]
    fn an_incomplete_proxy_jump_is_shown_and_not_imported_as_a_direct_connection() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    ProxyJump bastion
",
        );
        let web = host(&preview, "web");
        assert_eq!(web.outcome(), ImportOutcome::Blocked);
        assert!(!web.is_importable());
        assert_eq!(web.jumps().len(), 1, "the hop is still in the preview");
        assert_eq!(web.jumps()[0].address(), "bastion");
        let note = web
            .notes()
            .iter()
            .find(|note| note.kind() == NoteKind::Proxy)
            .expect("the missing jump host is explained");
        assert_eq!(note.line(), 4);
        assert!(note.keyword().eq_ignore_ascii_case("ProxyJump"));
        assert!(note.reason().contains("not imported"), "{}", note.reason());
        assert!(preview.plan(&HostBook::new()).is_empty());
    }

    #[test]
    fn proxy_jump_proxy_command_and_forwards_enter_the_preview() {
        let preview = parse_preview(
            "\
Host bastion
    HostName bastion.example.org
    User jump

Host web
    HostName web.example.org
    User deploy
    ProxyJump bastion
    ProxyCommand nc %h %p

Host files
    HostName files.example.org
    User deploy
    LocalForward 8080 localhost:80
    RemoteForward 9090 db.internal:5432
    DynamicForward 1080

Host tunnel
    HostName tunnel.example.org
    User deploy
    ProxyCommand ssh -W %h:%p bastion
",
        );
        let web = host(&preview, "web");
        assert_eq!(
            web.outcome(),
            ImportOutcome::Blocked,
            "a host cannot keep both a jump and a command"
        );
        assert!(web.proxy_command().is_some());
        assert_eq!(web.jumps().len(), 1);
        assert!(!web.is_importable());

        let files = host(&preview, "files");
        assert_eq!(files.outcome(), ImportOutcome::Ready);
        assert!(files.is_importable());
        assert_eq!(files.forwards().len(), 3);
        assert_eq!(
            files.forwards()[0].kind(),
            shellune_core::ForwardKind::Local
        );
        assert_eq!(files.forwards()[0].listen_port(), 8080);
        assert_eq!(files.forwards()[0].listen_host(), "127.0.0.1");
        assert_eq!(files.forwards()[0].target_host(), Some("localhost"));
        assert_eq!(
            files.forwards()[1].kind(),
            shellune_core::ForwardKind::Remote
        );
        assert_eq!(
            files.forwards()[2].kind(),
            shellune_core::ForwardKind::Dynamic
        );
        assert!(files.forwards()[2].target_host().is_none());
        assert!(
            files.summary().contains("forward"),
            "the row names the forwards: {}",
            files.summary()
        );

        let tunnel = host(&preview, "tunnel");
        assert_eq!(tunnel.outcome(), ImportOutcome::Ready);
        assert_eq!(
            tunnel.proxy_command(),
            Some("ssh -W %h:%p bastion"),
            "tokens the chain expands are kept as the command"
        );
        assert!(tunnel.is_importable());

        let bastion = host(&preview, "bastion");
        assert!(bastion.is_importable());
        let plan = preview.plan(&HostBook::new());
        assert!(
            plan.edits()
                .iter()
                .any(|edit| edit.host().name() == "files"),
            "the forwarded host is saved"
        );
        assert!(
            plan.edits().iter().all(|edit| edit.host().name() != "web"),
            "the conflicting host is not saved as a direct connection"
        );
        let saved_tunnel = plan
            .edits()
            .iter()
            .find(|edit| edit.host().name() == "tunnel")
            .expect("the proxy command host is saved");
        assert_eq!(
            saved_tunnel.host().proxy_command(),
            Some("ssh -W %h:%p bastion")
        );
        let saved_web_missing = plan.edits().iter().find(|edit| edit.host().name() == "web");
        assert!(saved_web_missing.is_none());
        let saved_files = plan
            .edits()
            .iter()
            .find(|edit| edit.host().name() == "files")
            .expect("files");
        let web_was_blocked = host(&preview, "web");
        assert!(web_was_blocked.summary().contains("bastion"));
        assert_eq!(plan.forwards().len(), 3);
        assert_eq!(plan.forwards()[0].alias(), "files");
        let rule = plan.forwards()[0]
            .forward()
            .rule(saved_files.host().id())
            .expect("the local rule can be stored");
        assert_eq!(rule.kind(), shellune_core::ForwardKind::Local);
        assert_eq!(rule.listen().port().get(), 8080);
        assert_eq!(
            rule.target().expect("local has a target").host(),
            "localhost"
        );

        let linked = preview.plan(&HostBook::new());
        let saved_bastion = linked
            .edits()
            .iter()
            .find(|edit| edit.host().name() == "bastion")
            .expect("bastion");
        // web is blocked, so nothing points at bastion from that host.
        assert!(saved_bastion.host().jumps().is_empty());

        let only_jump = parse_preview(
            "\
Host bastion
    HostName bastion.example.org
    User jump

Host web
    HostName web.example.org
    User deploy
    ProxyJump bastion
",
        );
        let web = host(&only_jump, "web");
        assert_eq!(web.outcome(), ImportOutcome::Ready, "{:?}", web.notes());
        assert_eq!(web.jumps()[0].alias(), Some("bastion"));
        let plan = only_jump.plan(&HostBook::new());
        let web = plan
            .edits()
            .iter()
            .find(|edit| edit.host().name() == "web")
            .expect("web");
        let bastion = plan
            .edits()
            .iter()
            .find(|edit| edit.host().name() == "bastion")
            .expect("bastion");
        assert_eq!(web.host().jumps(), &[bastion.host().id()]);
        assert!(web.host().proxy_command().is_none());
    }

    #[test]
    fn a_proxy_on_a_pattern_no_host_matches_is_reported_instead_of_dropped() {
        // A pattern decides which aliases a block applies to. When nothing in the file
        // is covered, the proxy never reaches an imported host — but ssh would still
        // route `ssh box.corp` through it, so the file was not read completely and the
        // preview has to name the pattern and the line instead of claiming otherwise.
        let wildcard = parse_preview(
            "\
Host *.corp
    ProxyJump bastion

Host web
    HostName web.example.org
    User deploy
",
        );
        assert_eq!(
            host(&wildcard, "web").outcome(),
            ImportOutcome::Ready,
            "the unmatched block does not change the host it never reached"
        );
        assert!(
            wildcard.is_partial(),
            "a block that was skipped makes the result partial"
        );
        assert!(
            wildcard.limits().is_partial(),
            "the limitation belongs to the file, not to one host"
        );
        let note = wildcard
            .limits()
            .unmatched_proxy()
            .first()
            .unwrap_or_else(|| panic!("the skipped block is reported: {:?}", wildcard.limits()));
        assert_eq!(note.line(), 1, "the note points at the Host line");
        assert!(
            note.keyword().contains("*.corp"),
            "the pattern is named: {}",
            note.keyword()
        );
        assert_eq!(note.kind(), NoteKind::Proxy);
        assert!(
            note.reason().contains("ProxyJump") && note.reason().contains("line 1"),
            "{}",
            note.reason()
        );

        // A negation next to a wildcard that nothing matches is the same omission, and
        // the pattern is quoted the way the file spelled it.
        let negated = parse_preview(
            "\
Host !bad web*
    ProxyCommand nc %h %p

Host db
    HostName db.example.org
    User ops
",
        );
        assert!(negated.is_partial());
        let note = negated
            .limits()
            .unmatched_proxy()
            .first()
            .unwrap_or_else(|| panic!("the skipped block is reported: {:?}", negated.limits()));
        assert_eq!(note.line(), 1);
        assert_eq!(
            note.keyword(),
            "!bad web*",
            "the pattern that was skipped is named"
        );
        assert_eq!(note.kind(), NoteKind::Proxy);
        assert!(note.reason().contains("ProxyCommand"), "{}", note.reason());

        // A list of only negated patterns excludes every alias, so it reaches none.
        let only_negated = parse_preview(
            "\
Host !bad
    LocalForward 8080 localhost:80

Host db
    HostName db.example.org
    User ops
",
        );
        let note = only_negated
            .limits()
            .unmatched_proxy()
            .first()
            .unwrap_or_else(|| {
                panic!("the skipped block is reported: {:?}", only_negated.limits())
            });
        assert_eq!(note.keyword(), "!bad");
        assert_eq!(
            note.kind(),
            NoteKind::Forwarding,
            "a forwarding is reported as a forwarding"
        );
        assert!(note.reason().contains("LocalForward"), "{}", note.reason());

        // And a block that does reach an alias is not listed here: it blocks that host
        // instead, which the host row already says.
        let reached = parse_preview(
            "\
Host web*
    ProxyJump bastion

Host web1
    HostName web1.example.org
    User deploy
",
        );
        assert_eq!(host(&reached, "web1").outcome(), ImportOutcome::Blocked);
        assert!(
            reached.limits().unmatched_proxy().is_empty(),
            "a reachable block is reported on the host, not twice"
        );
    }

    #[test]
    fn include_and_match_make_the_result_explicitly_partial() {
        let preview = parse_preview(
            "\
Include config.d/*

Host web
    HostName web.example.org
    User deploy

Match host db
    User dbuser
",
        );
        assert_eq!(preview.limits().include_lines(), &[1]);
        assert_eq!(preview.limits().match_lines(), &[7]);
        assert!(preview.is_partial());

        let web = host(&preview, "web");
        assert_eq!(web.outcome(), ImportOutcome::NeedsReview);
        assert!(
            web.is_importable(),
            "an Include makes the result partial, it does not invent a proxy"
        );
        assert_eq!(
            web.user(),
            Some("deploy"),
            "an unevaluated Match block is never applied on a guess"
        );
        let include = web
            .notes()
            .iter()
            .find(|note| note.kind() == NoteKind::Include)
            .expect("the Include is explained");
        assert_eq!(include.line(), 1);
        assert!(
            web.notes()
                .iter()
                .any(|note| note.kind() == NoteKind::Match && note.keyword() == "Match"),
            "the unevaluated Match block is named: {:?}",
            web.notes()
        );

        // A Match block can hold the proxy, and then no host is offered, because the
        // conditions could apply to it.
        let conditional = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy

Match exec \"true\"
    ProxyJump bastion
",
        );
        let web = host(&conditional, "web");
        assert_eq!(web.outcome(), ImportOutcome::Blocked);
        assert!(
            web.notes()
                .iter()
                .any(|note| note.kind() == NoteKind::Proxy && note.reason().contains("Match"))
        );
        assert_eq!(conditional.blocked_count(), 1);
        assert!(conditional.plan(&HostBook::new()).is_empty());
    }

    #[test]
    fn a_token_or_an_invalid_value_is_not_silently_replaced() {
        let preview = parse_preview(
            "\
Host web
    HostName %h.example.org
    User %r
    Port 70000
    IdentityFile %d/.ssh/id_ed25519
",
        );
        let web = host(&preview, "web");
        assert_eq!(web.address(), None, "a token is not an address");
        assert_eq!(web.user(), None, "a token is not a user");
        assert!(!web.is_importable());
        assert_eq!(web.port(), 22, "an invalid port falls back to the default");
        assert_eq!(web.identity_file(), None);
        let tokens: Vec<&ImportNote> = web
            .notes()
            .iter()
            .filter(|note| note.kind() == NoteKind::Token)
            .collect();
        assert_eq!(tokens.len(), 3, "{:?}", web.notes());
        assert!(tokens.iter().any(|note| note.reason().contains("%h")));
        assert!(
            web.notes().iter().any(
                |note| note.kind() == NoteKind::InvalidValue && note.reason().contains("70000")
            ),
            "the range failure names the value: {:?}",
            web.notes()
        );

        let doubled = parse_preview("Host web\n    HostName web.example.org\n    User de%%ploy\n");
        let web = host(&doubled, "web");
        assert_eq!(
            web.user(),
            Some("de%%ploy"),
            "a doubled percent is a literal percent"
        );
        assert_eq!(web.outcome(), ImportOutcome::Ready);
    }

    #[test]
    fn a_value_with_whitespace_is_refused_rather_than_trimmed_into_something_else() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org # a trailing note is not a comment to ssh
    User deploy
",
        );
        let web = host(&preview, "web");
        assert_eq!(
            web.address(),
            None,
            "the whole value is kept, and it is not an address"
        );
        assert!(!web.is_importable());
        assert!(
            web.notes()
                .iter()
                .any(|note| note.kind() == NoteKind::InvalidValue && note.line() == 2)
        );
    }

    #[test]
    fn a_malformed_file_fails_with_its_path_and_line() {
        for (text, line) in [
            ("Host web\n    User deploy\nHost\n", 3),
            ("Host web\n    Port\n", 2),
            ("Host web\n    User \"unbalanced\n", 2),
            ("Host *\n    User deploy\nMatch\n", 3),
        ] {
            let error = ImportPreview::from_text("C:/Users/example/.ssh/config", text)
                .expect_err("the file is malformed");
            assert_eq!(
                error.line(),
                Some(line),
                "the failure names the line: {error}"
            );
            assert_eq!(
                error.path(),
                Path::new("C:/Users/example/.ssh/config"),
                "the failure names the file"
            );
            assert!(
                error.to_string().contains("line"),
                "the message carries the location: {error}"
            );
        }
    }

    #[test]
    fn a_host_line_needs_at_least_one_alias() {
        let error = ImportPreview::from_text("config", "Host   \n    User deploy\n")
            .expect_err("an empty Host line is malformed");
        assert_eq!(error.line(), Some(1));
        assert!(error.reason().contains("Host"));
    }

    #[test]
    fn quoting_keeps_a_path_with_a_space_in_one_piece() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    IdentityFile \"C:/keys/work key.pem\"
",
        );
        let web = host(&preview, "web");
        assert_eq!(web.identity_file(), Some("C:/keys/work key.pem"));
        assert_eq!(web.outcome(), ImportOutcome::Ready);
    }

    #[test]
    fn the_plan_converts_importable_hosts_into_editable_records() {
        let preview = parse_preview(SAMPLE);
        let book = HostBook::new();
        let plan = preview.plan(&book);
        assert_eq!(plan.len(), 2);
        assert!(!plan.is_empty());
        assert!(plan.skipped().is_empty());

        let mut next = book.clone();
        assert_eq!(plan.apply(&mut next).expect("the hosts are created"), 2);
        let web = next
            .hosts()
            .iter()
            .find(|host| host.name() == "web")
            .expect("web was created")
            .clone();
        assert_eq!(web.target(), "deploy@web.example.org:2222");
        assert_eq!(web.identity_file(), Some("~/.ssh/id_ed25519"));

        // An imported record is an ordinary host: editing it changes what the next
        // connection would use.
        let mut edited = web.clone();
        edited
            .set_address("web2.example.org")
            .expect("a valid address");
        edited.set_port(2201).expect("a valid port");
        next.update(edited, web.id()).expect("the edit applies");
        let reloaded = next.host(web.id()).expect("the record is still there");
        assert_eq!(reloaded.target(), "deploy@web2.example.org:2201");
    }

    #[test]
    fn a_name_that_is_already_saved_is_skipped_with_a_reason() {
        let preview = parse_preview(SAMPLE);
        let existing = HostConfig::new("web", "old.example.org", 22, "root").expect("valid host");
        let book = HostBook::from_hosts(vec![existing]).expect("distinct hosts");
        let plan = preview.plan(&book);
        assert_eq!(plan.len(), 1, "only the host without a namesake is offered");
        assert_eq!(plan.skipped().len(), 1);
        assert_eq!(plan.skipped()[0].alias(), "web");
        assert!(
            plan.skipped()[0].reason().contains("already named"),
            "the reason says why: {}",
            plan.skipped()[0].reason()
        );
    }

    #[test]
    fn a_skipped_namesake_still_plans_its_forwarding_rules() {
        let preview = parse_preview(
            "\
Host web
    HostName web.example.org
    User deploy
    LocalForward 8080 localhost:80
    RemoteForward 9090 localhost:90
    DynamicForward 1080
",
        );
        let existing = HostConfig::new("web", "old.example.org", 22, "root").expect("valid host");
        let book = HostBook::from_hosts(vec![existing]).expect("distinct hosts");
        let plan = preview.plan(&book);
        assert!(plan.is_empty(), "the saved name is not created again");
        assert_eq!(plan.skipped().len(), 1);
        assert_eq!(plan.forwards().len(), 3);
        assert!(plan.forwards().iter().all(|rule| rule.alias() == "web"));
    }

    #[test]
    fn an_unreadable_file_and_a_directory_fail_with_the_path() {
        let missing = std::env::temp_dir().join("shellune-does-not-exist-config");
        let error = ImportPreview::from_file(&missing).expect_err("a missing file cannot be read");
        assert_eq!(error.path(), missing.as_path());
        assert_eq!(error.line(), None);
        assert!(
            !error.reason().is_empty(),
            "the operating system reason is kept"
        );

        let directory = std::env::temp_dir();
        let error = ImportPreview::from_file(&directory).expect_err("a directory is not a file");
        assert!(error.reason().contains("directory"));
    }

    #[test]
    fn a_tilde_is_resolved_against_the_home_directory() {
        let home = home_directory();
        let expected = match home.clone() {
            Some(home) => home.join(".ssh").join("config"),
            None => PathBuf::from("~/.ssh/config"),
        };
        assert_eq!(resolve_path(DEFAULT_CONFIG_TEXT), expected);
        if let Some(home) = home {
            assert_eq!(
                default_config_path(),
                Some(home.join(".ssh").join("config"))
            );
            assert_eq!(resolve_path("~/file"), home.join("file"));
        }
        assert_eq!(
            resolve_path("relative/config"),
            PathBuf::from("relative/config")
        );
    }
}
