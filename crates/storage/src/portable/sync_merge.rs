//! The record-by-record merge of two synced documents against the last one both
//! sides agreed on.
//!
//! When this machine and the cloud both changed their data since the last sync,
//! comparing only the two of them cannot tell an edit from the value it replaced,
//! or a record one side deleted from one the other side added. The document both
//! held after the last successful sync - the *base* - can. Every record (matched by
//! its `id`) and every field is merged three ways:
//!
//! * a side that left a value as the base had it takes the other side's value;
//! * two sides that made the same change agree;
//! * two different changes to one field, or an edit on one side against a
//!   deletion on the other, are a [`DocumentConflict`] - never decided silently.
//!
//! A conflict names where it is: the kind of record, the record's name, the field
//! and both values, so the person deciding sees exactly what differs. Each one is
//! answered on its own ([`MergeSide`]); until every one is answered no merged
//! document is produced and nothing may be written.
//!
//! The recent-connection log is not merged here: it keeps the later entry per
//! host ([`super::merge_recent_connections`]). The sections of other platforms,
//! the vault block and the stamps are the cloud's: this machine only writes its
//! own platform's section and the shared records.
//!
//! Nothing here is secret: the document holds no password (they travel in the
//! encrypted vault), so the values a conflict shows may be drawn and logged.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Map, Value};

use super::rebase::{repair_groups, repair_hosts};
use super::{Platform, PortableError, ShelluneData, merge_recent_connections};

/// The part of the data a conflict is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConflictSection {
    /// An SSH host.
    Host,
    /// A connection of another protocol (RDP, VNC, serial, ...).
    Connection,
    /// The favourites list.
    Favorites,
    /// The group tree.
    Groups,
    /// A forwarding rule.
    Forward,
    /// A saved command.
    Snippet,
    /// An accepted server identity.
    KnownHost,
    /// A shared preference.
    Preferences,
    /// A setting of this platform only (key bindings, text size, a host's
    /// machine-specific file).
    ThisPlatform,
    /// Anything this build does not name.
    Other,
}

/// Which side of a conflict is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MergeSide {
    /// This machine's value.
    Local,
    /// The cloud's value.
    Remote,
}

/// One place both sides changed differently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentConflict {
    /// A stable identifier of the place: record ids and field names, the same
    /// on every attempt as long as the data is the same. An answer names it.
    pub key: String,
    /// The part of the data it is in.
    pub section: ConflictSection,
    /// The record's name (or its id when it has none); `None` for a setting
    /// that is not a record.
    pub record: Option<String>,
    /// The field inside the record, dotted for nested fields; empty when the
    /// whole record differs (one side deleted it).
    pub field: String,
    /// This machine's value; `None` when this machine deleted it or never had it.
    pub local: Option<String>,
    /// The cloud's value; `None` when the cloud deleted it or never had it.
    pub remote: Option<String>,
}

/// The result of one merge.
#[derive(Clone, Debug)]
pub struct DocumentMerge {
    /// The merged document, when every conflict was answered (or there was
    /// none). It is the cloud's document with the merged shared records, this
    /// platform's merged section and the merged recent connections.
    pub document: Option<ShelluneData>,
    /// Every conflict found, answered or not, in a stable order.
    pub conflicts: Vec<DocumentConflict>,
    /// How many conflicts are still without an answer.
    pub unanswered: usize,
}

/// Merges `local` and `remote` against `base` (see the module documentation).
///
/// `answers` keeps one side per [`DocumentConflict::key`]; an answer naming a
/// place that is no longer a conflict is ignored.
///
/// # Errors
///
/// Returns [`PortableError::Encode`] when a document cannot be turned into
/// JSON, and [`PortableError::Corrupt`] (or another reading error) when the
/// merged records do not form a valid document - two answers that cannot hold
/// together, such as a host kept in a group the other side removed and nothing
/// can repair. Nothing is written either way.
pub fn merge_documents(
    base: &ShelluneData,
    local: &ShelluneData,
    remote: &ShelluneData,
    platform: Platform,
    answers: &BTreeMap<String, MergeSide>,
) -> Result<DocumentMerge, PortableError> {
    let base_value = without_recents(to_value(base)?);
    let local_value = without_recents(to_value(local)?);
    let remote_value = without_recents(to_value(remote)?);

    let mut merger = Merger {
        answers,
        conflicts: Vec::new(),
        unanswered: 0,
    };
    let mut merged = remote_value.clone();
    let mut path = Vec::new();

    // The shared records.
    path.push(Segment::Key("shared".to_owned()));
    let shared = merger.merge(
        &mut path,
        base_value.get("shared"),
        local_value.get("shared"),
        remote_value.get("shared"),
    );
    path.pop();
    set(&mut merged, "shared", shared);

    // This platform's own section; the other platforms' are the cloud's.
    let name = platform.as_str();
    let section = |value: &Value| {
        value
            .get("platforms")
            .and_then(|all| all.get(name))
            .cloned()
    };
    path.push(Segment::Key("platforms".to_owned()));
    path.push(Segment::Key(name.to_owned()));
    let own = merger.merge(
        &mut path,
        section(&base_value).as_ref(),
        section(&local_value).as_ref(),
        section(&remote_value).as_ref(),
    );
    path.clear();
    if let Some(object) = merged.as_object_mut() {
        let platforms = object
            .entry("platforms")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(platforms) = platforms.as_object_mut() {
            match own {
                Some(own) => {
                    platforms.insert(name.to_owned(), own);
                }
                None => {
                    platforms.remove(name);
                }
            }
        }
    }

    merger
        .conflicts
        .sort_by(|a, b| (a.section, &a.record, &a.field).cmp(&(b.section, &b.record, &b.field)));
    if merger.unanswered > 0 {
        return Ok(DocumentMerge {
            document: None,
            conflicts: merger.conflicts,
            unanswered: merger.unanswered,
        });
    }

    // The merge of two valid books can leave a favourite of a deleted record or
    // a group whose parent the other side removed; both are repaired the way the
    // window's own rebase repairs them.
    if let Some(hosts) = merged.pointer_mut("/shared/hosts") {
        repair_hosts(hosts);
    }
    if let Some(groups) = merged.pointer_mut("/shared/groups") {
        repair_groups(groups);
    }
    let bytes = serde_json::to_vec(&merged).map_err(|error| PortableError::Encode {
        reason: error.to_string(),
    })?;
    let document = ShelluneData::from_json(&bytes)?;
    let recents = merge_recent_connections(
        local.shared().hosts().recents(),
        remote.shared().hosts().recents(),
    );
    let document = document.with_recents(&recents)?;
    Ok(DocumentMerge {
        document: Some(document),
        conflicts: merger.conflicts,
        unanswered: 0,
    })
}

fn to_value(document: &ShelluneData) -> Result<Value, PortableError> {
    serde_json::to_value(document).map_err(|error| PortableError::Encode {
        reason: error.to_string(),
    })
}

/// The document without its recent-connection log, which merges on its own.
fn without_recents(mut value: Value) -> Value {
    if let Some(Value::Array(recents)) = value.pointer_mut("/shared/hosts/recents") {
        recents.clear();
    }
    value
}

fn set(target: &mut Value, key: &str, value: Option<Value>) {
    if let Some(object) = target.as_object_mut() {
        match value {
            Some(value) => {
                object.insert(key.to_owned(), value);
            }
            None => {
                object.remove(key);
            }
        }
    }
}

/// One step of the path to a value.
#[derive(Clone, Debug)]
enum Segment {
    /// An object key.
    Key(String),
    /// A record of a list, by its id, with the name it goes by.
    Record { id: String, label: String },
}

struct Merger<'a> {
    answers: &'a BTreeMap<String, MergeSide>,
    conflicts: Vec<DocumentConflict>,
    unanswered: usize,
}

impl Merger<'_> {
    /// The three-way merge of one value; `None` is "absent".
    fn merge(
        &mut self,
        path: &mut Vec<Segment>,
        base: Option<&Value>,
        local: Option<&Value>,
        remote: Option<&Value>,
    ) -> Option<Value> {
        if local == remote || local == base {
            return remote.cloned();
        }
        if remote == base {
            return local.cloned();
        }
        match (local, remote) {
            (Some(Value::Object(local)), Some(Value::Object(remote))) => {
                let base = match base {
                    Some(Value::Object(base)) => Some(base),
                    _ => None,
                };
                let mut out = Map::new();
                let keys: Vec<String> = remote
                    .keys()
                    .chain(local.keys().filter(|key| !remote.contains_key(*key)))
                    .cloned()
                    .collect();
                for key in keys {
                    path.push(Segment::Key(key.clone()));
                    let merged = self.merge(
                        path,
                        base.and_then(|base| base.get(&key)),
                        local.get(&key),
                        remote.get(&key),
                    );
                    path.pop();
                    if let Some(value) = merged {
                        out.insert(key, value);
                    }
                }
                Some(Value::Object(out))
            }
            (Some(Value::Array(local_items)), Some(Value::Array(remote_items))) => {
                let base_items = match base {
                    Some(Value::Array(base)) => base.as_slice(),
                    _ => &[],
                };
                if let (Some(base_ids), Some(local_ids), Some(remote_ids)) = (
                    record_ids(base_items),
                    record_ids(local_items),
                    record_ids(remote_items),
                ) {
                    return Some(Value::Array(self.merge_records(
                        path,
                        base_ids.into_iter().zip(base_items).collect(),
                        local_ids.into_iter().zip(local_items).collect(),
                        remote_ids.into_iter().zip(remote_items).collect(),
                    )));
                }
                if local_items.iter().all(is_plain) && remote_items.iter().all(is_plain) {
                    return Some(Value::Array(merge_set(
                        base_items,
                        local_items,
                        remote_items,
                    )));
                }
                self.conflict(path, local, remote)
            }
            _ => self.conflict(path, local, remote),
        }
    }

    /// Merges two lists of records by id, keeping the cloud's order and adding
    /// this machine's new records after it.
    fn merge_records(
        &mut self,
        path: &mut Vec<Segment>,
        base: HashMap<String, &Value>,
        local: Vec<(String, &Value)>,
        remote: Vec<(String, &Value)>,
    ) -> Vec<Value> {
        let local_by_id: HashMap<&str, &Value> = local
            .iter()
            .map(|(id, value)| (id.as_str(), *value))
            .collect();
        let remote_ids: HashSet<&str> = remote.iter().map(|(id, _)| id.as_str()).collect();
        let mut out = Vec::with_capacity(remote.len().max(local.len()));
        let order = remote
            .iter()
            .map(|(id, value)| (id, Some(*value), local_by_id.get(id.as_str()).copied()))
            .chain(
                local
                    .iter()
                    .filter(|(id, _)| !remote_ids.contains(id.as_str()))
                    .map(|(id, value)| (id, None, Some(*value))),
            );
        for (id, remote_value, local_value) in order {
            let base_value = base.get(id).copied();
            let label = [local_value, remote_value, base_value]
                .into_iter()
                .flatten()
                .find_map(record_label)
                .unwrap_or_else(|| id.trim_matches('"').to_owned());
            path.push(Segment::Record {
                id: id.clone(),
                label,
            });
            if let Some(value) = self.merge(path, base_value, local_value, remote_value) {
                out.push(value);
            }
            path.pop();
        }
        // A record both sides deleted is gone; one deleted on one side only and
        // untouched on the other went through `merge` above as absent.
        out
    }

    /// Records one conflict and returns the answered side's value, or the
    /// cloud's as a placeholder until it is answered.
    fn conflict(
        &mut self,
        path: &[Segment],
        local: Option<&Value>,
        remote: Option<&Value>,
    ) -> Option<Value> {
        let key = path_key(path);
        let answer = self.answers.get(&key).copied();
        let (section, record, field) = describe(path);
        self.conflicts.push(DocumentConflict {
            key,
            section,
            record,
            field,
            local: local.map(show),
            remote: remote.map(show),
        });
        match answer {
            Some(MergeSide::Local) => local.cloned(),
            Some(MergeSide::Remote) => remote.cloned(),
            None => {
                self.unanswered += 1;
                remote.cloned()
            }
        }
    }
}

/// Whether a list item merges as a set member: a value without an `id`.
fn is_plain(value: &Value) -> bool {
    !value.is_object() || value.get("id").is_none()
}

/// A set merge: the cloud's items, minus what this machine removed, plus what
/// this machine added.
fn merge_set(base: &[Value], local: &[Value], remote: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = remote
        .iter()
        .filter(|item| !(base.contains(item) && !local.contains(item)))
        .cloned()
        .collect();
    for item in local {
        if !base.contains(item) && !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

/// The `id` of a record, when it has a plain one.
fn record_id(value: &Value) -> Option<String> {
    let id = value.as_object()?.get("id")?;
    (id.is_string() || id.is_number()).then(|| id.to_string())
}

/// The ids of a list of records, when every item has one and no two share it.
/// An empty list qualifies, so a list one side emptied still merges by record.
fn record_ids(list: &[Value]) -> Option<Vec<String>> {
    let ids: Vec<String> = list.iter().map(record_id).collect::<Option<_>>()?;
    let unique: HashSet<&String> = ids.iter().collect();
    (unique.len() == ids.len()).then_some(ids)
}

/// The name a record goes by, for a person.
fn record_label(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    for field in ["name", "title", "label"] {
        if let Some(text) = object.get(field).and_then(Value::as_str)
            && !text.trim().is_empty()
        {
            return Some(text.to_owned());
        }
    }
    // A forwarding rule or an identity: where it points.
    let host = object.get("host").and_then(Value::as_str)?;
    Some(match object.get("port").and_then(Value::as_u64) {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

/// The stable key of a place: object keys and record ids, joined.
fn path_key(path: &[Segment]) -> String {
    path.iter()
        .map(|segment| match segment {
            Segment::Key(key) => key.clone(),
            Segment::Record { id, .. } => format!("[{}]", id.trim_matches('"')),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The section, the record and the field a path names.
fn describe(path: &[Segment]) -> (ConflictSection, Option<String>, String) {
    let keys: Vec<&str> = path
        .iter()
        .filter_map(|segment| match segment {
            Segment::Key(key) => Some(key.as_str()),
            Segment::Record { .. } => None,
        })
        .collect();
    let section = match keys.as_slice() {
        ["shared", "hosts", "hosts", ..] => ConflictSection::Host,
        ["shared", "hosts", "connections", ..] => ConflictSection::Connection,
        ["shared", "hosts", "favorites", ..] => ConflictSection::Favorites,
        ["shared", "groups", ..] => ConflictSection::Groups,
        ["shared", "forwards", ..] => ConflictSection::Forward,
        ["shared", "snippets", ..] => ConflictSection::Snippet,
        ["shared", "known_hosts", ..] => ConflictSection::KnownHost,
        ["shared", "preferences", ..] => ConflictSection::Preferences,
        ["platforms", ..] => ConflictSection::ThisPlatform,
        _ => ConflictSection::Other,
    };
    let last_record = path
        .iter()
        .rposition(|segment| matches!(segment, Segment::Record { .. }));
    let record = last_record.and_then(|index| match &path[index] {
        Segment::Record { label, .. } => Some(label.clone()),
        Segment::Key(_) => None,
    });
    // The field: the keys after the record, or after the section's own keys
    // for a setting that is not a record.
    let skip = match last_record {
        Some(index) => index + 1,
        None => match section {
            ConflictSection::Preferences | ConflictSection::Groups => 2,
            ConflictSection::ThisPlatform => 2,
            _ => path.len().saturating_sub(1),
        },
    };
    let field = path
        .iter()
        .skip(skip)
        .map(|segment| match segment {
            Segment::Key(key) => key.as_str(),
            Segment::Record { label, .. } => label.as_str(),
        })
        .collect::<Vec<_>>()
        .join(".");
    // A per-host overlay of this platform names the host by id; the caller
    // finds its name in the records.
    let record = record.or_else(|| match (section, keys.as_slice()) {
        (ConflictSection::ThisPlatform, [_, _, "hosts", id, ..]) => Some((*id).to_owned()),
        _ => None,
    });
    (section, record, field)
}

/// A value as a person reads it: text as it is, everything else as compact
/// JSON, cut to a line.
fn show(value: &Value) -> String {
    const LONGEST: usize = 120;
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Null => "—".to_owned(),
        other => other.to_string(),
    };
    if text.chars().count() > LONGEST {
        let mut cut: String = text.chars().take(LONGEST - 1).collect();
        cut.push('…');
        cut
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use shellune_core::HostConfig;

    use super::*;
    use crate::book::HostBook;
    use crate::portable::{ExportOptions, LocalState};

    fn document(hosts: &[HostConfig]) -> ShelluneData {
        let mut book = HostBook::new();
        for host in hosts {
            book.insert(host.clone()).expect("a valid host");
        }
        let state = LocalState {
            hosts: book,
            ..LocalState::default()
        };
        ShelluneData::export_from_vault(&state, &ExportOptions::new(Platform::Linux), None, None)
            .expect("the state exports")
            .document
    }

    fn host(name: &str, port: u16) -> HostConfig {
        HostConfig::new(name, "10.0.0.1", port, "root").expect("a valid host")
    }

    fn renamed(host: &HostConfig, name: &str) -> HostConfig {
        let mut host = host.clone();
        host.set_name(name).expect("a valid name");
        host
    }

    fn with_port(host: &HostConfig, port: u16) -> HostConfig {
        let mut host = host.clone();
        host.set_port(u32::from(port)).expect("a valid port");
        host
    }

    fn names(document: &ShelluneData) -> Vec<String> {
        let mut names: Vec<String> = document
            .shared()
            .hosts()
            .hosts()
            .iter()
            .map(|host| host.name().to_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn changes_to_different_records_merge_without_a_conflict() {
        let web = host("web", 22);
        let db = host("db", 22);
        let base = document(&[web.clone(), db.clone()]);
        let local = document(&[renamed(&web, "web-1"), db.clone()]);
        let new = host("cache", 22);
        let remote = document(&[web.clone(), with_port(&db, 2222), new]);
        let merge = merge_documents(&base, &local, &remote, Platform::Linux, &BTreeMap::new())
            .expect("the documents merge");
        assert!(merge.conflicts.is_empty(), "{:?}", merge.conflicts);
        let merged = merge.document.expect("nothing to answer");
        assert_eq!(names(&merged), vec!["cache", "db", "web-1"]);
        let db_port = merged
            .shared()
            .hosts()
            .hosts()
            .iter()
            .find(|host| host.name() == "db")
            .map(|host| host.port().get());
        assert_eq!(db_port, Some(2222));
    }

    #[test]
    fn different_changes_to_one_field_name_the_record_the_field_and_both_values() {
        let web = host("web", 22);
        let base = document(std::slice::from_ref(&web));
        let local = document(&[with_port(&web, 2200)]);
        let remote = document(&[with_port(&web, 2300)]);
        let merge = merge_documents(&base, &local, &remote, Platform::Linux, &BTreeMap::new())
            .expect("the documents merge");
        assert!(merge.document.is_none(), "an open conflict writes nothing");
        assert_eq!(merge.unanswered, 1);
        let conflict = &merge.conflicts[0];
        assert_eq!(conflict.section, ConflictSection::Host);
        assert_eq!(conflict.record.as_deref(), Some("web"));
        assert_eq!(conflict.field, "port");
        assert_eq!(conflict.local.as_deref(), Some("2200"));
        assert_eq!(conflict.remote.as_deref(), Some("2300"));

        let answers = BTreeMap::from([(conflict.key.clone(), MergeSide::Local)]);
        let answered = merge_documents(&base, &local, &remote, Platform::Linux, &answers)
            .expect("the documents merge");
        let merged = answered.document.expect("every conflict is answered");
        assert_eq!(merged.shared().hosts().hosts()[0].port().get(), 2200);
    }

    #[test]
    fn an_edit_against_a_deletion_is_a_conflict_about_the_whole_record() {
        let web = host("web", 22);
        let base = document(std::slice::from_ref(&web));
        let local = document(&[]);
        let remote = document(&[with_port(&web, 2222)]);
        let merge = merge_documents(&base, &local, &remote, Platform::Linux, &BTreeMap::new())
            .expect("the documents merge");
        assert_eq!(merge.conflicts.len(), 1);
        let conflict = &merge.conflicts[0];
        assert_eq!(conflict.record.as_deref(), Some("web"));
        assert_eq!(conflict.field, "");
        assert_eq!(conflict.local, None, "this machine deleted it");
        assert!(conflict.remote.is_some());

        let answers = BTreeMap::from([(conflict.key.clone(), MergeSide::Local)]);
        let merged = merge_documents(&base, &local, &remote, Platform::Linux, &answers)
            .expect("the documents merge")
            .document
            .expect("answered");
        assert!(names(&merged).is_empty(), "keeping the deletion deletes it");
    }

    #[test]
    fn the_same_change_on_both_sides_is_no_conflict() {
        let web = host("web", 22);
        let base = document(std::slice::from_ref(&web));
        let both = document(&[with_port(&web, 2222)]);
        let merge = merge_documents(&base, &both, &both, Platform::Linux, &BTreeMap::new())
            .expect("the documents merge");
        assert!(merge.conflicts.is_empty());
        assert!(merge.document.is_some());
    }

    #[test]
    fn a_conflict_key_is_stable_between_attempts() {
        let web = host("web", 22);
        let base = document(std::slice::from_ref(&web));
        let local = document(&[with_port(&web, 1)]);
        let remote = document(&[with_port(&web, 2)]);
        let first = merge_documents(&base, &local, &remote, Platform::Linux, &BTreeMap::new())
            .expect("merges");
        let second = merge_documents(&base, &local, &remote, Platform::Linux, &BTreeMap::new())
            .expect("merges");
        assert_eq!(first.conflicts, second.conflicts);
        assert!(first.conflicts[0].key.contains(&web.id().to_string()));
    }
}
