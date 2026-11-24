//! The saved records: hosts, their group labels, the recent-connection log and
//! the favourite marks.
//!
//! The SSH hosts are the host list this build has always written, and the
//! connections of the other protocols are kept beside it with an explicit protocol
//! tag, so an older file still reads and an SSH record keeps its exact shape.
//!
//! One book serves every view. Search returns references into the same host list
//! the group view and the recent view use, so an edited address is what every view
//! shows; there is no second copy that a view could keep stale.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize, Serializer};
use shellune_core::{ConnectionRecord, GroupPath, HostConfig, HostId, JumpFault, jump_faults};

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

/// A saved connection this build could not read, kept exactly as it was written.
///
/// A newer build may add a field or a protocol to a connection record. Refusing the
/// whole file for that would leave every other record unusable, so the record is set
/// aside instead: it is not listed, cannot be dialled, and is written back unchanged
/// so the build that understands it still finds it. A record that names a
/// credential is not set aside; such a file is refused as a whole, because keeping
/// the record would mean writing that value again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadableConnection {
    id: Option<HostId>,
    name: Option<String>,
    protocol: Option<String>,
    reason: String,
    stored: serde_json::Value,
}

impl UnreadableConnection {
    fn new(stored: serde_json::Value, reason: String) -> Self {
        let text = |key: &str| {
            stored
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        Self {
            id: stored
                .get("id")
                .and_then(|id| serde_json::from_value(id.clone()).ok()),
            name: text("name"),
            protocol: text("protocol"),
            reason,
            stored,
        }
    }

    /// The identifier the record carries, when it has a readable one.
    #[must_use]
    pub fn id(&self) -> Option<HostId> {
        self.id
    }

    /// The name the record carries, when it has one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The protocol tag as written, when there is one.
    #[must_use]
    pub fn protocol(&self) -> Option<&str> {
        self.protocol.as_deref()
    }

    /// Why this build could not read the record, as the reader reported it.
    ///
    /// Never a credential: a record that names one fails the whole file instead.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The record exactly as it was written.
    pub(crate) fn stored(&self) -> &serde_json::Value {
        &self.stored
    }
}

/// Everything the user saved.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "HostBookWire")]
pub struct HostBook {
    version: u32,
    hosts: Vec<HostConfig>,
    /// The saved connections whose protocol is not SSH (R2, R18).
    ///
    /// Kept beside the host list rather than replacing it: the SSH record is the one
    /// older builds wrote and the one every existing consumer takes, so a file from
    /// an older build stays readable and an SSH connection keeps its exact fields.
    /// Each record here carries its protocol explicitly, which is what lets one
    /// record of an unknown protocol be reported instead of loaded.
    ///
    /// Absent from a file that has no such connection, so a book of SSH hosts is
    /// written byte for byte as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    connections: Vec<ConnectionRecord>,
    recents: Vec<RecentConnection>,
    /// The records the user marked as favourites, in the order they were marked.
    ///
    /// Identifiers rather than copies, so a favourite is the record every other view
    /// shows; an SSH host and a connection of another protocol are marked the same
    /// way. Absent from a file without favourites, so such a book is written byte for
    /// byte as before and an older file reads as having none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    favorites: Vec<HostId>,
    /// Connection records this build could not read, written back after the
    /// readable ones (see [`UnreadableConnection`]).
    #[serde(skip)]
    unreadable: Vec<UnreadableConnection>,
}

/// The stored shape of a host book.
///
/// Connection records are read one at a time, so one record this build cannot
/// understand is set aside rather than failing the file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostBookWire {
    version: u32,
    hosts: Vec<HostConfig>,
    #[serde(default)]
    connections: Vec<serde_json::Value>,
    recents: Vec<RecentConnection>,
    #[serde(default)]
    favorites: Vec<HostId>,
}

impl TryFrom<HostBookWire> for HostBook {
    type Error = String;

    fn try_from(wire: HostBookWire) -> Result<Self, Self::Error> {
        let mut connections = Vec::with_capacity(wire.connections.len());
        let mut unreadable = Vec::new();
        for stored in wire.connections {
            if let Some(key) = crate::portable::find_secret_key(&stored) {
                let id = stored
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("without an id");
                return Err(format!(
                    "the connection {id} contains the field \"{key}\", which cannot be stored. Its value was not kept"
                ));
            }
            match serde_json::from_value::<ConnectionRecord>(stored.clone()) {
                Ok(record) => connections.push(record),
                Err(error) => unreadable.push(UnreadableConnection::new(stored, error.to_string())),
            }
        }
        Ok(Self {
            version: wire.version,
            hosts: wire.hosts,
            connections,
            recents: wire.recents,
            favorites: wire.favorites,
            unreadable,
        })
    }
}

impl Serialize for HostBook {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        /// One entry of the connection list: a record, or one kept as written.
        #[derive(Serialize)]
        #[serde(untagged)]
        enum Stored<'a> {
            Record(&'a ConnectionRecord),
            Kept(&'a serde_json::Value),
        }

        /// The document in the order this build has always written it.
        #[derive(Serialize)]
        struct Document<'a> {
            version: u32,
            hosts: &'a [HostConfig],
            #[serde(skip_serializing_if = "Vec::is_empty")]
            connections: Vec<Stored<'a>>,
            recents: &'a [RecentConnection],
            #[serde(skip_serializing_if = "<[HostId]>::is_empty")]
            favorites: &'a [HostId],
        }

        Document {
            version: self.version,
            hosts: &self.hosts,
            connections: self
                .connections
                .iter()
                .map(Stored::Record)
                .chain(
                    self.unreadable
                        .iter()
                        .map(|kept| Stored::Kept(kept.stored())),
                )
                .collect(),
            recents: &self.recents,
            favorites: &self.favorites,
        }
        .serialize(serializer)
    }
}

impl Default for HostBook {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            hosts: Vec::new(),
            connections: Vec::new(),
            recents: Vec::new(),
            favorites: Vec::new(),
            unreadable: Vec::new(),
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

    /// The saved connections of any protocol that were reached, newest first.
    ///
    /// A deleted record is not listed: the recent view and quick shortcuts must never
    /// offer a connection that no longer has a saved record.
    #[must_use]
    pub fn recent_records(&self) -> Vec<(ConnectionRecord, u64, u32)> {
        let mut recent: Vec<(ConnectionRecord, u64, u32)> = self
            .recents
            .iter()
            .filter_map(|recent| {
                self.connection(recent.host())
                    .map(|record| (record, recent.last_connected_unix(), recent.connections()))
            })
            .collect();
        recent.sort_by(|left, right| {
            right
                .1
                .cmp(&left.1)
                .then_with(|| left.0.name().cmp(right.0.name()))
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
        self.favorites.retain(|favorite| *favorite != id);
        Ok(self.hosts.remove(index))
    }

    /// The identifiers of the favourite records, in the order they were marked.
    #[must_use]
    pub fn favorites(&self) -> &[HostId] {
        &self.favorites
    }

    /// Whether a record is marked as a favourite.
    #[must_use]
    pub fn is_favorite(&self, id: HostId) -> bool {
        self.favorites.contains(&id)
    }

    /// Marks a saved record of any protocol as a favourite, or clears the mark.
    ///
    /// Returns whether the book changed, so marking a favourite twice (or clearing a
    /// mark that is not there) reports nothing to save. A newly marked record goes to
    /// the end of the list.
    ///
    /// # Errors
    ///
    /// Returns [`BookError::UnknownHost`] when no record with that identifier is
    /// saved, which leaves the book as it was.
    pub fn set_favorite(&mut self, id: HostId, favorite: bool) -> Result<bool, BookError> {
        if !self.contains_connection(id) {
            return Err(BookError::UnknownHost { id });
        }
        let marked = self.is_favorite(id);
        if favorite == marked {
            return Ok(false);
        }
        if favorite {
            self.favorites.push(id);
        } else {
            self.favorites.retain(|saved| *saved != id);
        }
        Ok(true)
    }

    /// Flips the favourite mark of a saved record and returns the new state.
    ///
    /// # Errors
    ///
    /// Returns [`BookError::UnknownHost`] when no record with that identifier is
    /// saved.
    pub fn toggle_favorite(&mut self, id: HostId) -> Result<bool, BookError> {
        let favorite = !self.is_favorite(id);
        self.set_favorite(id, favorite)?;
        Ok(favorite)
    }

    /// The favourite records of any protocol, in the order they were marked.
    ///
    /// An SSH host answers as [`ConnectionRecord::Ssh`], the same way
    /// [`Self::connection`] presents it.
    #[must_use]
    pub fn favorite_hosts(&self) -> Vec<ConnectionRecord> {
        self.favorites
            .iter()
            .filter_map(|id| self.connection(*id))
            .collect()
    }

    /// Every saved connection whose protocol is not SSH, in saved order.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionRecord] {
        &self.connections
    }

    /// The connection records this build could not read, in saved order.
    ///
    /// None of them is in [`Self::records`]; each is written back unchanged when
    /// the book is saved, so a newer build that wrote it still finds it.
    #[must_use]
    pub fn unreadable_connections(&self) -> &[UnreadableConnection] {
        &self.unreadable
    }

    /// The identifiers of the records set aside.
    pub(crate) fn unreadable_ids(&self) -> impl Iterator<Item = HostId> + '_ {
        self.unreadable.iter().filter_map(UnreadableConnection::id)
    }

    /// One saved connection of any protocol.
    ///
    /// An SSH host answers as [`ConnectionRecord::Ssh`], so a caller that lists
    /// connections does not need a second lookup for the common case.
    #[must_use]
    pub fn connection(&self, id: HostId) -> Option<ConnectionRecord> {
        if let Some(host) = self.host(id) {
            return Some(ConnectionRecord::Ssh(host.clone()));
        }
        self.connections
            .iter()
            .find(|connection| connection.id() == id)
            .cloned()
    }

    /// How many connections are saved, whatever their protocol.
    #[must_use]
    pub fn connection_count(&self) -> usize {
        self.hosts.len() + self.connections.len()
    }

    /// Every saved connection: the SSH hosts first, then the other protocols.
    ///
    /// The two lists are kept apart because the SSH record is the shape older builds
    /// wrote; this is the one place that presents them as what the user sees.
    #[must_use]
    pub fn records(&self) -> Vec<ConnectionRecord> {
        let mut records: Vec<ConnectionRecord> = self
            .hosts
            .iter()
            .cloned()
            .map(ConnectionRecord::Ssh)
            .collect();
        records.extend(self.connections.iter().cloned());
        records
    }

    /// Whether either list already holds this identifier.
    #[must_use]
    pub fn contains_connection(&self, id: HostId) -> bool {
        self.host(id).is_some()
            || self
                .connections
                .iter()
                .any(|connection| connection.id() == id)
    }

    /// Adds a connection of any protocol.
    ///
    /// An SSH connection is stored as a host record, so neither list can hold the
    /// same connection twice and every existing consumer of `hosts` keeps working.
    ///
    /// # Errors
    ///
    /// Returns [`BookError::DuplicateHost`] when the identifier is already saved.
    pub fn insert_connection(&mut self, connection: ConnectionRecord) -> Result<(), BookError> {
        if self.contains_connection(connection.id())
            || self.unreadable_ids().any(|id| id == connection.id())
        {
            return Err(BookError::DuplicateHost {
                id: connection.id(),
            });
        }
        match connection {
            ConnectionRecord::Ssh(host) => self.hosts.push(host),
            other => self.connections.push(other),
        }
        Ok(())
    }

    /// Replaces a saved connection, keeping its position when its protocol stays.
    ///
    /// A change from SSH to another protocol (or back) moves the record to the other
    /// list, which is a different place in the document; the connection keeps its
    /// identifier either way. Leaving SSH also drops the recency record, because a
    /// history of SSH sign-ins is not a history of desktop sessions.
    ///
    /// # Errors
    ///
    /// Returns [`BookError::UnknownHost`] when no connection with that identifier is
    /// saved.
    pub fn update_connection(
        &mut self,
        connection: ConnectionRecord,
        id: HostId,
    ) -> Result<(), BookError> {
        let host_slot = self.hosts.iter().position(|host| host.id() == id);
        let connection_slot = self.connections.iter().position(|saved| saved.id() == id);
        if host_slot.is_none() && connection_slot.is_none() {
            return Err(BookError::UnknownHost { id });
        }
        // The identifier is the connection's identity in the credential store and in
        // the workspace, so a record that arrives under a different one would silently
        // become a second connection. Refusing says which edit was wrong.
        if connection.id() != id {
            return Err(BookError::Inconsistent {
                reason: format!(
                    "the connection {} cannot replace {}, because they are different connections",
                    connection.id(),
                    id
                ),
            });
        }
        match connection {
            ConnectionRecord::Ssh(host) => {
                if let Some(slot) = host_slot {
                    self.hosts[slot] = host;
                } else {
                    self.hosts.push(host);
                }
                if let Some(slot) = connection_slot {
                    self.connections.remove(slot);
                    self.recents.retain(|recent| recent.host() != id);
                }
            }
            other => {
                if let Some(slot) = connection_slot {
                    if self.connections[slot].protocol() != other.protocol() {
                        self.recents.retain(|recent| recent.host() != id);
                    }
                    self.connections[slot] = other;
                } else {
                    self.connections.push(other);
                }
                if let Some(slot) = host_slot {
                    self.hosts.remove(slot);
                    self.recents.retain(|recent| recent.host() != id);
                }
            }
        }
        Ok(())
    }

    /// Removes a saved connection of any protocol.
    ///
    /// # Errors
    ///
    /// Returns [`BookError::UnknownHost`] when no connection with that identifier is
    /// saved.
    pub fn remove_connection(&mut self, id: HostId) -> Result<ConnectionRecord, BookError> {
        if let Some(slot) = self.hosts.iter().position(|host| host.id() == id) {
            self.recents.retain(|recent| recent.host() != id);
            self.favorites.retain(|favorite| *favorite != id);
            return Ok(ConnectionRecord::Ssh(self.hosts.remove(slot)));
        }
        if let Some(slot) = self
            .connections
            .iter()
            .position(|connection| connection.id() == id)
        {
            self.recents.retain(|recent| recent.host() != id);
            self.favorites.retain(|favorite| *favorite != id);
            return Ok(self.connections.remove(slot));
        }
        Err(BookError::UnknownHost { id })
    }

    /// Connections of any protocol matching a search phrase (R2).
    ///
    /// The phrase is matched the way the host list already matches it, and an SSH
    /// connection is also found by the word "ssh"; a desktop or serial connection is
    /// found by its protocol name and by what it would reach. An empty phrase keeps
    /// every connection, so clearing the field restores the list.
    #[must_use]
    pub fn search_connections(&self, phrase: &str) -> Vec<ConnectionRecord> {
        let phrase = phrase.trim().to_lowercase();
        self.records()
            .into_iter()
            .filter(|connection| match connection {
                ConnectionRecord::Ssh(host) => {
                    host_matches(host, &phrase) || connection.matches(&phrase)
                }
                other => other.matches(&phrase),
            })
            .collect()
    }

    /// Records that a saved connection of any protocol was really reached.
    ///
    /// # Errors
    ///
    /// Returns `BookError::UnknownHost` when the identifier names no saved record,
    /// which stops a connection to a deleted record from being recorded.
    pub fn record_connection(&mut self, id: HostId, at_unix: u64) -> Result<(), BookError> {
        if !self.contains_connection(id) {
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
    /// another place, and the ones beside it are no business of the change. A
    /// connection of another protocol is in a group the same way, so
    /// [`Self::connections_under_group`] answers the same question for those records.
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

    /// The connections of another protocol in one group and in every group inside it.
    ///
    /// Listed apart from [`Self::hosts_under_group`] because the two are stored in
    /// different lists, but they are moved by the same group change: a group that is
    /// renamed or deleted has to carry both, or a connection keeps a path no tree holds.
    #[must_use]
    pub fn connections_under_group(&self, path: &GroupPath) -> Vec<HostId> {
        self.connections
            .iter()
            .filter(|connection| {
                connection
                    .group_path()
                    .is_some_and(|current| current.starts_with(path))
            })
            .map(ConnectionRecord::id)
            .collect()
    }

    /// Moves the given hosts into one group, or out of every group.
    ///
    /// Returns how many hosts actually changed group, so moving a selection that is
    /// already where it is being moved to reports nothing. All or nothing: an
    /// identifier that names no saved host leaves the book exactly as it was, because a
    /// half-applied move would make the counts the user confirmed disagree with the tree.
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
            if !self.contains_connection(*id) {
                return Err(BookError::UnknownHost { id: *id });
            }
        }
        // A selection can be as large as the list, so membership is a set lookup: one
        // pass over five thousand hosts must not compare every host with every id.
        let selected: HashSet<HostId> = ids.iter().copied().collect();
        let mut changed = 0;
        for host in &mut self.hosts {
            if !selected.contains(&host.id()) || already_in(host, destination) {
                continue;
            }
            host.set_group_path(destination.cloned());
            changed += 1;
        }
        // A desktop or serial connection is moved by the same operation, so a group
        // deletion cannot leave one behind in a group that no longer exists.
        for index in 0..self.connections.len() {
            let connection = self.connections[index].clone();
            // A connection already where it is being moved to is not a change, the same
            // way [`already_in`] answers for a host.
            if !selected.contains(&connection.id())
                || connection.group_path().as_ref() == destination
            {
                continue;
            }
            self.connections[index] = connection.with_group_path(destination.cloned());
            changed += 1;
        }
        Ok(changed)
    }

    /// The hosts whose saved label has no group path this build can draw.
    ///
    /// A label with a control character, or one longer than a group name, is not a
    /// group the tree can hold, so those hosts cannot be counted in a group and cannot
    /// be moved with one. They are listed instead of disappearing from a count: the
    /// caller shows them, and the label itself stays exactly as it was saved.
    #[must_use]
    pub fn legacy_group_labels(&self) -> Vec<LegacyGroupLabel> {
        self.hosts
            .iter()
            .filter_map(|host| {
                host.group_label_without_path()
                    .map(|label| LegacyGroupLabel {
                        host: host.id(),
                        host_name: host.name().to_owned(),
                        label: label.to_owned(),
                    })
            })
            .collect()
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
        // A connection of another protocol is in a group the same way a host is, so a
        // rename has to move it too: leaving it behind would show the old name in the
        // list and hide it from a filter on the new one.
        let mut pending_connections: Vec<(usize, GroupPath)> = Vec::new();
        for (index, connection) in self.connections.iter().enumerate() {
            let Some(current) = connection.group_path() else {
                continue;
            };
            if !current.starts_with(from) {
                continue;
            }
            let moved = current
                .rebase(from, to)
                .ok_or_else(|| BookError::Inconsistent {
                    reason: format!(
                        "renaming {from} to {to} would put the connection {} below the deepest group that can be shown",
                        connection.name()
                    ),
                })?;
            pending_connections.push((index, moved));
        }
        let changed = pending.len() + pending_connections.len();
        for (index, moved) in pending {
            self.hosts[index].set_group_path(Some(moved));
        }
        for (index, moved) in pending_connections {
            let connection = self.connections[index].clone();
            self.connections[index] = connection.with_group_path(Some(moved));
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
        for connection in &self.connections {
            // An SSH connection is a host: a file that lists one here asks this build
            // to read the same record two ways, which it does not write and will not
            // guess at.
            if connection.protocol().is_ssh() {
                return Err(format!(
                    "the connection {} is stored as a non-SSH record, so the file is not one this build wrote",
                    connection.id()
                ));
            }
            if !ids.insert(connection.id()) {
                return Err(format!(
                    "two records share the id {}, so the file is not a valid host book",
                    connection.id()
                ));
            }
        }
        // A record set aside is still a record: its identifier is taken, and its
        // recent connection and favourite mark stay with it.
        for id in self.unreadable_ids() {
            if !ids.insert(id) {
                return Err(format!(
                    "two records share the id {id}, so the file is not a valid host book"
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
        let mut marked = HashSet::new();
        for favorite in &self.favorites {
            if !ids.contains(favorite) {
                return Err(format!(
                    "a favourite refers to the unknown record {favorite}"
                ));
            }
            if !marked.insert(*favorite) {
                return Err(format!(
                    "the record {favorite} is marked as a favourite twice"
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

/// Whether a host is already exactly in one group.
///
/// `None` is "no group at all", which is different from a host that still carries a
/// label: clearing that label is a change, so it is counted.
fn already_in(host: &HostConfig, destination: Option<&GroupPath>) -> bool {
    match destination {
        Some(path) => host.group_path().as_ref() == Some(path),
        None => host.group_path().is_none() && host.group().is_none(),
    }
}

/// A saved group label that has no group path this build can draw.
///
/// The label is kept as it was written, so a view can still show it beside the host;
/// what it cannot do is place the host in the group tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyGroupLabel {
    host: HostId,
    host_name: String,
    label: String,
}

impl LegacyGroupLabel {
    /// The host that carries the label.
    #[must_use]
    pub fn host(&self) -> HostId {
        self.host
    }

    /// The name of that host, so a preview can name it without another lookup.
    #[must_use]
    pub fn host_name(&self) -> &str {
        &self.host_name
    }

    /// The label as it was saved.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
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
