//! The saved records: hosts, their group labels and the recent-connection log.
//!
//! One book serves every view. Search returns references into the same host list
//! the group view and the recent view use, so an edited address is what every view
//! shows; there is no second copy that a view could keep stale.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use shellune_core::{GroupPath, HostConfig, HostId, JumpFault, jump_faults};

use crate::error::BookError;

/// The format this build writes.
///
/// The version is part of the document rather than of the file name so a future
/// build can read an older file and either migrate it or refuse it by name.
pub const FORMAT_VERSION: u32 = 1;

/// One successful connection, recorded only after a connection really happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentConnection {
    host: HostId,
    last_connected_unix: u64,
    connections: u32,
}

impl RecentConnection {
    /// The host that was reached.
    #[must_use]
    pub fn host(self) -> HostId {
        self.host
    }

    /// Seconds since the Unix epoch of the most recent connection.
    #[must_use]
    pub fn last_connected_unix(self) -> u64 {
        self.last_connected_unix
    }

    /// How many times this host was reached.
    #[must_use]
    pub fn connections(self) -> u32 {
        self.connections
    }
}

/// A host together with how recently it was reached.
#[derive(Clone, Copy, Debug)]
pub struct RecentHost<'a> {
    /// The host record the connection used.
    pub host: &'a HostConfig,
    /// Seconds since the Unix epoch of the most recent successful connection.
    pub last_connected_unix: u64,
    /// How many successful connections were recorded.
    pub connections: u32,
}

/// The hosts that share a group label.
#[derive(Clone, Debug, Default)]
pub struct Group<'a> {
    label: Option<String>,
    hosts: Vec<&'a HostConfig>,
}

impl<'a> Group<'a> {
    /// The label, or `None` for hosts without one.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The hosts in this group, in saved order.
    #[must_use]
    pub fn hosts(&self) -> &[&'a HostConfig] {
        &self.hosts
    }

    /// How many hosts the group holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    /// Whether the group is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

/// A validated change to the saved hosts.
///
/// Either a new record or a replacement for an existing one, so the store knows
/// whether an identifier may be created or has to already exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEdit {
    host: HostConfig,
    replaces: Option<HostId>,
}

impl HostEdit {
    /// An edit that creates a record.
    #[must_use]
    pub fn create(host: HostConfig) -> Self {
        Self {
            host,
            replaces: None,
        }
    }

    /// An edit that replaces an existing record.
    #[must_use]
    pub fn replace(host: HostConfig) -> Self {
        Self {
            replaces: Some(host.id()),
            host,
        }
    }

    /// The record to save.
    #[must_use]
    pub fn host(&self) -> &HostConfig {
        &self.host
    }

    /// The record this edit replaces, when it is not a creation.
    #[must_use]
    pub fn replaces(&self) -> Option<HostId> {
        self.replaces
    }

    /// The identifier that will exist after the edit.
    #[must_use]
    pub fn id(&self) -> HostId {
        self.host.id()
    }
}

/// Everything the user saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostBook {
    version: u32,
    hosts: Vec<HostConfig>,
    recents: Vec<RecentConnection>,
}

impl Default for HostBook {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            hosts: Vec::new(),
            recents: Vec::new(),
        }
    }
}

impl HostBook {
    /// An empty book, ready to receive records.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A book seeded with existing records, as a restored section would pass them.
    ///
    /// # Errors
    ///
    /// Returns `BookError::DuplicateHost` when two records share an identifier.
    pub fn from_hosts(hosts: Vec<HostConfig>) -> Result<Self, BookError> {
        let book = Self {
            hosts,
            ..Self::default()
        };
        book.validate()
            .map_err(|reason| BookError::Inconsistent { reason })?;
        Ok(book)
    }

    /// The format version the records were read from.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// All hosts, in the order they were saved.
    #[must_use]
    pub fn hosts(&self) -> &[HostConfig] {
        &self.hosts
    }

    /// One host by identifier.
    #[must_use]
    pub fn host(&self, id: HostId) -> Option<&HostConfig> {
        self.hosts.iter().find(|host| host.id() == id)
    }

    /// The recorded connections, newest first.
    #[must_use]
    pub fn recents(&self) -> &[RecentConnection] {
        &self.recents
    }

    /// The recency record of one host.
    #[must_use]
    pub fn recent_of(&self, id: HostId) -> Option<RecentConnection> {
        self.recents
            .iter()
            .find(|recent| recent.host() == id)
            .copied()
    }

    /// The hosts that were reached, newest first.
    ///
    /// A record whose host was deleted is not listed: the recent view must not
    /// offer a connection to an address that no longer has a record.
    #[must_use]
    pub fn recent_hosts(&self) -> Vec<RecentHost<'_>> {
        let mut recent: Vec<RecentHost<'_>> = self
            .recents
            .iter()
            .filter_map(|recent| {
                self.host(recent.host()).map(|host| RecentHost {
                    host,
                    last_connected_unix: recent.last_connected_unix(),
                    connections: recent.connections(),
                })
            })
            .collect();
        recent.sort_by(|left, right| {
            right
                .last_connected_unix
                .cmp(&left.last_connected_unix)
                .then_with(|| left.host.name().cmp(right.host.name()))
        });
        recent
    }

    /// Applies one validated edit.
    ///
    /// # Errors
    ///
    /// Returns `BookError::DuplicateHost` when a creation reuses a saved
    /// identifier and `BookError::UnknownHost` when a replacement names a host
    /// that is not saved.
    pub fn apply(&mut self, edit: &HostEdit) -> Result<(), BookError> {
        match edit.replaces() {
            Some(id) => self.update(edit.host().clone(), id),
            None => self.insert(edit.host().clone()),
        }
    }

    /// Adds a host.
    ///
    /// # Errors
    ///
    /// Returns `BookError::DuplicateHost` when the identifier is already saved.
    pub fn insert(&mut self, host: HostConfig) -> Result<(), BookError> {
        if self.host(host.id()).is_some() {
            return Err(BookError::DuplicateHost { id: host.id() });
        }
        self.hosts.push(host);
        Ok(())
    }

    /// Replaces a saved host, keeping its position and its connection history.
    ///
    /// # Errors
    ///
    /// Returns `BookError::UnknownHost` when no host with that identifier exists.
    pub fn update(&mut self, host: HostConfig, id: HostId) -> Result<(), BookError> {
        let Some(slot) = self.hosts.iter_mut().find(|saved| saved.id() == id) else {
            return Err(BookError::UnknownHost { id });
        };
        *slot = host;
        Ok(())
    }

    /// Removes a host and the recency record that belongs to it.
    ///
    /// # Errors
    ///
    /// Returns `BookError::UnknownHost` when no host with that identifier exists.
    pub fn remove(&mut self, id: HostId) -> Result<HostConfig, BookError> {
        let Some(index) = self.hosts.iter().position(|host| host.id() == id) else {
            return Err(BookError::UnknownHost { id });
        };
        self.recents.retain(|recent| recent.host() != id);
        Ok(self.hosts.remove(index))
    }

    /// Records that a host was really reached.
    ///
    /// # Errors
    ///
    /// Returns `BookError::UnknownHost` when the identifier names no saved host,
    /// which is what stops a connection to a deleted host from being recorded.
    pub fn record_connection(&mut self, id: HostId, at_unix: u64) -> Result<(), BookError> {
        if self.host(id).is_none() {
            return Err(BookError::UnknownHost { id });
        }
        match self.recents.iter_mut().find(|recent| recent.host() == id) {
            Some(recent) => {
                recent.last_connected_unix = at_unix;
                recent.connections = recent.connections.saturating_add(1);
            }
            None => self.recents.push(RecentConnection {
                host: id,
                last_connected_unix: at_unix,
                connections: 1,
            }),
        }
        Ok(())
    }

    /// Hosts matching a search phrase, in saved order.
    ///
    /// The phrase is matched case-insensitively against the name, the address, the
    /// user and the group label; an empty phrase matches everything, so clearing
    /// the search field restores the whole list.
    #[must_use]
    pub fn search(&self, phrase: &str) -> Vec<&HostConfig> {
        let phrase = phrase.trim().to_lowercase();
        self.hosts
            .iter()
            .filter(|host| phrase.is_empty() || matches(host, &phrase))
            .collect()
    }

    /// Groups the given hosts by label, with named groups alphabetically first and
    /// the hosts without a label last.
    ///
    /// Taking the hosts as an argument keeps the grouping and the search over the
    /// same records: a grouped view of a filtered list is still a grouped view of
    /// the saved hosts.
    #[must_use]
    pub fn group<'a>(hosts: impl IntoIterator<Item = &'a HostConfig>) -> Vec<Group<'a>> {
        let mut named: BTreeMap<String, Group<'a>> = BTreeMap::new();
        let mut ungrouped = Group::default();
        for host in hosts {
            match host.group_text().filter(|label| !label.trim().is_empty()) {
                Some(label) => {
                    named
                        .entry(label.to_lowercase())
                        .or_insert_with(|| Group {
                            label: Some(label.clone()),
                            hosts: Vec::new(),
                        })
                        .hosts
                        .push(host);
                }
                None => ungrouped.hosts.push(host),
            }
        }
        let mut groups: Vec<Group<'a>> = named.into_values().collect();
        if !ungrouped.is_empty() {
            groups.push(ungrouped);
        }
        groups
    }

    /// The group labels in display order, for a group chooser.
    #[must_use]
    pub fn group_labels(&self) -> Vec<String> {
        let mut labels: Vec<String> = self
            .hosts
            .iter()
            .filter_map(HostConfig::group_text)
            .map(|label| label.trim().to_owned())
            .filter(|label| !label.is_empty())
            .collect();
        labels.sort_unstable();
        labels.dedup();
        labels
    }

    /// The hosts exactly in one group.
    #[must_use]
    pub fn hosts_in_group(&self, path: &GroupPath) -> Vec<HostId> {
        self.hosts
            .iter()
            .filter(|host| host.group_path().as_ref() == Some(path))
            .map(HostConfig::id)
            .collect()
    }

    /// The hosts in one group and in every group inside it.
    ///
    /// Deleting or renaming a group moves exactly these hosts: the ones above it are
    /// another place, and the ones beside it are no business of the change.
    #[must_use]
    pub fn hosts_under_group(&self, path: &GroupPath) -> Vec<HostId> {
        self.hosts
            .iter()
            .filter(|host| {
                host.group_path()
                    .is_some_and(|current| current.starts_with(path))
            })
            .map(HostConfig::id)
            .collect()
    }

    /// Moves the given hosts into one group, or out of every group.
    ///
    /// Returns how many hosts changed. All or nothing: an identifier that names no
    /// saved host leaves the book exactly as it was, because a half-applied move would
    /// make the counts the user confirmed disagree with the tree.
    ///
    /// # Errors
    ///
    /// Returns [`BookError::UnknownHost`] for the first identifier that is not saved.
    pub fn reassign_group(
        &mut self,
        ids: &[HostId],
        destination: Option<&GroupPath>,
    ) -> Result<usize, BookError> {
        for id in ids {
            if self.host(*id).is_none() {
                return Err(BookError::UnknownHost { id: *id });
            }
        }
        // A selection can be as large as the list, so membership is a set lookup: one
        // pass over five thousand hosts must not compare every host with every id.
        let selected: HashSet<HostId> = ids.iter().copied().collect();
        let mut changed = 0;
        for host in &mut self.hosts {
            if selected.contains(&host.id()) {
                host.set_group_path(destination.cloned());
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// Rewrites the path of every host inside a renamed group.
    ///
    /// Returns how many hosts changed. The hosts keep their identifiers, addresses,
    /// jump chains and credential references: only the group moves.
    ///
    /// # Errors
    ///
    /// Returns [`BookError::Inconsistent`] when a moved path would be deeper than the
    /// limit, which leaves the book as it was.
    pub fn rename_group_paths(
        &mut self,
        from: &GroupPath,
        to: &GroupPath,
    ) -> Result<usize, BookError> {
        let mut pending: Vec<(usize, GroupPath)> = Vec::new();
        for (index, host) in self.hosts.iter().enumerate() {
            let Some(current) = host.group_path() else {
                continue;
            };
            if !current.starts_with(from) {
                continue;
            }
            let moved = current
                .rebase(from, to)
                .ok_or_else(|| BookError::Inconsistent {
                    reason: format!(
                        "renaming {from} to {to} would put the host {} below the deepest group that can be shown",
                        host.name()
                    ),
                })?;
            pending.push((index, moved));
        }
        let changed = pending.len();
        for (index, moved) in pending {
            self.hosts[index].set_group_path(Some(moved));
        }
        Ok(changed)
    }

    /// Jump-chain problems in this book.
    ///
    /// A cycle or a hop that names a deleted host is local to that chain. The
    /// book stays loaded, and reporting the fault does not open a connection.
    #[must_use]
    pub fn jump_faults(&self) -> Vec<JumpFault> {
        jump_faults(&self.hosts)
    }

    /// The first jump problem that would block connecting `target`.
    ///
    /// `None` means the chain resolves. Nothing is dialed.
    #[must_use]
    pub fn connection_jump_fault(&self, target: HostId) -> Option<JumpFault> {
        shellune_core::connection_jump_fault(&self.hosts, target)
    }

    /// Checks the invariants a stored document must satisfy.
    fn validate(&self) -> Result<(), String> {
        let mut ids = HashSet::new();
        for host in &self.hosts {
            if !ids.insert(host.id()) {
                return Err(format!(
                    "two hosts share the id {}, so the file is not a valid host book",
                    host.id()
                ));
            }
        }
        let mut seen = HashSet::new();
        for recent in &self.recents {
            if !ids.contains(&recent.host()) {
                return Err(format!(
                    "a recent connection refers to the unknown host {}",
                    recent.host()
                ));
            }
            if !seen.insert(recent.host()) {
                return Err(format!(
                    "the host {} has two recent-connection records",
                    recent.host()
                ));
            }
        }
        Ok(())
    }

    /// Rejects a document that breaks the invariants, used after deserialization.
    pub(crate) fn ensure_valid(&self) -> Result<(), String> {
        self.validate()
    }
}

/// Whether one host answers a search phrase.
///
/// Public because the recent view filters the same records: one matcher means a
/// phrase that finds a host in the list finds it in the recent view too.
#[must_use]
pub fn host_matches(host: &HostConfig, phrase: &str) -> bool {
    let phrase = phrase.trim().to_lowercase();
    phrase.is_empty() || matches(host, &phrase)
}

/// Whether one host answers a lower-cased search phrase.
fn matches(host: &HostConfig, phrase: &str) -> bool {
    let haystacks = [
        host.name().to_lowercase(),
        host.address().to_lowercase(),
        host.user().to_lowercase(),
        host.group_text().unwrap_or_default().to_lowercase(),
    ];
    haystacks.iter().any(|value| value.contains(phrase))
}
