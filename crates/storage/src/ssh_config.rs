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
//! block, an Include, an OpenSSH token inside a value, a directive this build does
//! not apply, and every directive that could change the target or the path to it
//! (ProxyJump, ProxyCommand and the three forwarding directives). A host that a
//! proxy or a forwarding could apply to is Blocked and is never offered for import,
//! so the application cannot open a direct connection that the file would have
//! routed somewhere else. Nothing here adds a jump host, a proxy or a forwarding
//! feature: the only thing the importer does with them is refuse to pretend.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use shellune_core::{DEFAULT_SSH_PORT, HostConfig};

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
    /// A proxy, a proxy command or a forwarding could apply, so the host is not
    /// imported: connecting directly could take a path the file never intended.
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
    /// Whether the note describes something that stops the host from being
    /// imported as a direct connection.
    #[must_use]
    pub fn blocks_import(self) -> bool {
        matches!(self, Self::Proxy | Self::Forwarding)
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

/// One host the file describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportedHost {
    alias: String,
    address: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    identity_file: Option<String>,
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
        self.outcome.is_offered() && self.user.is_some() && self.address.is_some()
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
        let hosts = aliases
            .into_iter()
            .map(|(alias, line)| resolve_host(&blocks, &alias, line))
            .collect();
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
    /// the saved record is the one the user can already edit.
    #[must_use]
    pub fn plan(&self, book: &HostBook) -> ImportPlan {
        let mut taken: HashSet<String> = book
            .hosts()
            .iter()
            .map(|host| host.name().to_lowercase())
            .collect();
        let mut edits = Vec::new();
        let mut skipped = Vec::new();

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
            taken.insert(name);
            edits.push(HostEdit::create(record));
        }

        ImportPlan { edits, skipped }
    }
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
    Include,
    Ignored,
}

/// Classifies one directive by keyword.
///
/// The three blocks are not interchangeable: a proxy or a forwarding is the reason
/// a host is not imported, an Include means files were not read, and everything else
/// is a directive this build simply does not apply.
fn classify(keyword: &str) -> Directive {
    match keyword {
        "hostname" => Directive::HostName,
        "user" => Directive::User,
        "port" => Directive::Port,
        "identityfile" => Directive::IdentityFile,
        "proxyjump" | "proxycommand" => Directive::Proxy,
        "localforward" | "remoteforward" | "dynamicforward" => Directive::Forwarding,
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
                    Directive::Proxy => {
                        blocked = true;
                        push_once(
                            &mut host.notes,
                            &mut reported,
                            ImportNote::new(
                                setting.line,
                                &setting.keyword,
                                NoteKind::Proxy,
                                format!(
                                    "line {}: {} sits inside a Match block that Shellune does not evaluate, so this host may not be a direct connection and was not imported",
                                    setting.line, setting.keyword
                                ),
                            ),
                        );
                    }
                    Directive::Forwarding => {
                        blocked = true;
                        push_once(
                            &mut host.notes,
                            &mut reported,
                            ImportNote::new(
                                setting.line,
                                &setting.keyword,
                                NoteKind::Forwarding,
                                format!(
                                    "line {}: {} sits inside a Match block that Shellune does not evaluate, so this host may use a forwarded port and was not imported",
                                    setting.line, setting.keyword
                                ),
                            ),
                        );
                    }
                    Directive::Include | Directive::Ignored => {}
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
                    blocked = true;
                    push_once(
                        &mut host.notes,
                        &mut reported,
                        ImportNote::new(
                            setting.line,
                            &setting.keyword,
                            NoteKind::Proxy,
                            format!(
                                "line {}: {} sends this connection through another machine, which this build does not do; the host was not imported so a direct connection cannot be opened by mistake",
                                setting.line, setting.keyword
                            ),
                        ),
                    );
                }
                Directive::Forwarding => {
                    blocked = true;
                    push_once(
                        &mut host.notes,
                        &mut reported,
                        ImportNote::new(
                            setting.line,
                            &setting.keyword,
                            NoteKind::Forwarding,
                            format!(
                                "line {}: {} opens a forwarded port, which this build does not manage; the host was not imported so its port is not used without a review",
                                setting.line, setting.keyword
                            ),
                        ),
                    );
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

    host.outcome = if blocked {
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
    ServerAliveInterval 30
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
    fn proxy_and_forwarding_directives_block_the_host() {
        for (directive, kind) in [
            ("ProxyJump bastion", NoteKind::Proxy),
            ("ProxyCommand ssh -W %h:%p bastion", NoteKind::Proxy),
            ("LocalForward 8080 localhost:80", NoteKind::Forwarding),
            ("RemoteForward 9090 localhost:90", NoteKind::Forwarding),
            ("DynamicForward 1080", NoteKind::Forwarding),
        ] {
            let text = format!(
                "Host web\n    HostName web.example.org\n    User deploy\n    {directive}\n"
            );
            let preview = parse_preview(&text);
            let web = host(&preview, "web");
            assert_eq!(
                web.outcome(),
                ImportOutcome::Blocked,
                "{directive} must block the host"
            );
            assert!(!web.is_importable(), "{directive} must not be imported");
            assert_eq!(preview.blocked_count(), 1);
            let note = web
                .notes()
                .iter()
                .find(|note| note.kind() == kind)
                .unwrap_or_else(|| panic!("{directive} is recorded as {kind:?}"));
            assert_eq!(note.line(), 4, "the note points at the directive");
            let keyword = directive.split(' ').next().unwrap_or_default();
            assert!(note.keyword().eq_ignore_ascii_case(keyword));

            // The plan never offers a blocked host, so no direct connection exists.
            let plan = preview.plan(&HostBook::new());
            assert!(plan.is_empty(), "{directive} must not reach the book");
        }
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
