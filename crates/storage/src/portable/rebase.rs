//! Edits made while another writer replaced the data files (R8, R10).
//!
//! A sync pull or a backup import writes the data files on a background thread, and
//! the window reads them again afterwards. An edit the window makes in between must
//! neither be written over the files the other writer just put there (that would
//! silently drop the records that arrived) nor be thrown away. So the window holds
//! such edits back, and once the other writer is done they are *rebased*: each
//! document the window changed is merged three ways —
//!
//! * `base`: the files as they were when the other writer started,
//! * `ours`: what the window holds now (`base` plus its edits),
//! * `theirs`: the files as the other writer left them —
//!
//! keeping every change of either side. Records are matched by their `id`; when both
//! sides changed the same field of the same record, the window's edit (the newer
//! intent) wins. A document whose combination would not be a valid file keeps the
//! other writer's version, and is reported, so nothing invalid is ever written and
//! nothing that arrived is overwritten.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use shellune_core::{GroupBook, GroupPath};

use super::{LocalData, LocalState, PortableError};
use crate::forwarding::ForwardStore;
use crate::group::GroupStore;
use crate::preferences::PreferencesStore;
use crate::snippets::SnippetStore;
use crate::store::HostStore;
use crate::trust::TrustStore;

/// The documents the window changed while the files were being replaced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Touched {
    /// `hosts.json`: hosts, other connections, recents, favourites.
    pub hosts: bool,
    /// `groups.json`.
    pub groups: bool,
    /// `forwards.json`.
    pub forwards: bool,
    /// `snippets.json`.
    pub snippets: bool,
    /// `known_hosts.json`.
    pub known_hosts: bool,
    /// `preferences.json`.
    pub preferences: bool,
}

impl Touched {
    /// Whether any document was changed.
    #[must_use]
    pub fn any(self) -> bool {
        self.hosts
            || self.groups
            || self.forwards
            || self.snippets
            || self.known_hosts
            || self.preferences
    }
}

/// What rebasing did.
#[derive(Clone, Debug)]
pub struct Rebased {
    /// The data as it is on disk afterwards.
    pub state: LocalState,
    /// Whether any file was written.
    pub written: bool,
    /// The documents whose edits could not be combined with the files on disk
    /// (named as [`LocalData::load`] names them). Their files were left as the
    /// other writer wrote them.
    pub dropped: Vec<&'static str>,
}

/// Names of the documents, as the load errors name them.
const HOSTS: &str = "saved hosts";
const GROUPS: &str = "host groups";
const FORWARDS: &str = "forwarding rules";
const SNIPPETS: &str = "saved commands";
const TRUST: &str = "trusted server identities";
const PREFERENCES: &str = "preferences";

impl LocalData {
    /// Re-applies the edits in `ours` (made since `base` was read) to the files as
    /// they are now, and writes every document that changed.
    ///
    /// Only the documents in `touched` are considered; the others are left as they
    /// are on disk.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Local`] when the files cannot be read, and
    /// [`PortableError::Write`] when one could not be written (the documents written
    /// before it stay written; each write is atomic).
    pub fn rebase(
        &self,
        base: &LocalState,
        ours: &LocalState,
        touched: Touched,
    ) -> Result<Rebased, PortableError> {
        let theirs = self.load()?;
        let (state, dropped) = rebase_state(base, ours, &theirs, touched);
        let root = self.root();
        let mut written = false;
        let failed =
            |path: std::path::PathBuf, error: &dyn std::fmt::Display| PortableError::Write {
                path,
                reason: error.to_string(),
            };
        if state.hosts != theirs.hosts {
            let store = HostStore::at(root);
            store
                .save(&state.hosts)
                .map_err(|error| failed(store.path(), &error))?;
            written = true;
        }
        if state.groups != theirs.groups {
            let store = GroupStore::at(root);
            store
                .save(&state.groups)
                .map_err(|error| failed(store.path(), &error))?;
            written = true;
        }
        if state.forwards != theirs.forwards {
            let store = ForwardStore::at(root);
            store
                .save(&state.forwards)
                .map_err(|error| failed(store.path(), &error))?;
            written = true;
        }
        if state.snippets != theirs.snippets {
            let store = SnippetStore::at(root);
            store
                .save(&state.snippets)
                .map_err(|error| failed(store.path(), &error))?;
            written = true;
        }
        if state.known_hosts != theirs.known_hosts {
            let store = TrustStore::at(root);
            store
                .save(&state.known_hosts)
                .map_err(|error| failed(store.path(), &error))?;
            written = true;
        }
        if state.preferences != theirs.preferences {
            let store = PreferencesStore::at(root);
            store
                .save(&state.preferences)
                .map_err(|error| failed(store.path(), &error))?;
            written = true;
        }
        Ok(Rebased {
            state,
            written,
            dropped,
        })
    }
}

/// Merges the touched documents of `ours` onto `theirs`; returns the result and the
/// documents whose edits had to be dropped.
#[must_use]
pub fn rebase_state(
    base: &LocalState,
    ours: &LocalState,
    theirs: &LocalState,
    touched: Touched,
) -> (LocalState, Vec<&'static str>) {
    let mut dropped = Vec::new();
    let mut state = theirs.clone();
    if touched.hosts {
        let merged = rebase_document(
            &base.hosts,
            &ours.hosts,
            &theirs.hosts,
            repair_hosts,
            crate::book::HostBook::ensure_valid,
        );
        take(HOSTS, merged, &mut state.hosts, &mut dropped);
    }
    if touched.groups {
        let merged = rebase_document(
            &base.groups,
            &ours.groups,
            &theirs.groups,
            repair_groups,
            GroupBook::validate,
        );
        take(GROUPS, merged, &mut state.groups, &mut dropped);
    }
    if touched.forwards {
        let merged = rebase_document(
            &base.forwards,
            &ours.forwards,
            &theirs.forwards,
            |_| {},
            shellune_core::ForwardBook::validate,
        );
        take(FORWARDS, merged, &mut state.forwards, &mut dropped);
    }
    if touched.snippets {
        let merged = rebase_document(
            &base.snippets,
            &ours.snippets,
            &theirs.snippets,
            |_| {},
            shellune_core::SnippetBook::validate,
        );
        take(SNIPPETS, merged, &mut state.snippets, &mut dropped);
    }
    if touched.known_hosts {
        let merged = rebase_document(
            &base.known_hosts,
            &ours.known_hosts,
            &theirs.known_hosts,
            |_| {},
            crate::trust::HostTrust::validate,
        );
        take(TRUST, merged, &mut state.known_hosts, &mut dropped);
    }
    if touched.preferences {
        let merged = rebase_document(
            &base.preferences,
            &ours.preferences,
            &theirs.preferences,
            |_| {},
            |_| Ok(()),
        );
        take(PREFERENCES, merged, &mut state.preferences, &mut dropped);
    }
    (state, dropped)
}

/// Puts a merged document in place, or names the document whose edits were dropped.
fn take<T>(label: &'static str, merged: Option<T>, slot: &mut T, dropped: &mut Vec<&'static str>) {
    match merged {
        Some(value) => *slot = value,
        None => dropped.push(label),
    }
}

/// Merges one document three ways; `None` when the result is not a valid document.
fn rebase_document<T>(
    base: &T,
    ours: &T,
    theirs: &T,
    repair: impl FnOnce(&mut Value),
    check: impl FnOnce(&T) -> Result<(), String>,
) -> Option<T>
where
    T: Serialize + DeserializeOwned + PartialEq + Clone,
{
    if ours == base {
        return Some(theirs.clone());
    }
    if theirs == base || ours == theirs {
        return Some(ours.clone());
    }
    let base = serde_json::to_value(base).ok()?;
    let ours = serde_json::to_value(ours).ok()?;
    let theirs = serde_json::to_value(theirs).ok()?;
    let mut merged = merge(Some(&base), Some(&ours), Some(&theirs))?;
    repair(&mut merged);
    let document: T = serde_json::from_value(merged).ok()?;
    check(&document).ok()?;
    Some(document)
}

/// The three-way merge of one JSON value. `None` is "absent".
///
/// A side that did not change takes the other side; objects merge field by field;
/// arrays of records with an `id` merge record by record, other arrays as sets; two
/// different changes to one scalar keep ours.
fn merge(base: Option<&Value>, ours: Option<&Value>, theirs: Option<&Value>) -> Option<Value> {
    if ours == base {
        return theirs.cloned();
    }
    if theirs == base || ours == theirs {
        return ours.cloned();
    }
    match (ours, theirs) {
        (Some(Value::Object(ours)), Some(Value::Object(theirs))) => {
            let base = match base {
                Some(Value::Object(base)) => Some(base),
                _ => None,
            };
            let mut out = Map::new();
            let keys = theirs
                .keys()
                .chain(ours.keys().filter(|key| !theirs.contains_key(*key)));
            for key in keys {
                let merged = merge(
                    base.and_then(|base| base.get(key)),
                    ours.get(key),
                    theirs.get(key),
                );
                if let Some(value) = merged {
                    out.insert(key.clone(), value);
                }
            }
            Some(Value::Object(out))
        }
        (Some(Value::Array(ours)), Some(Value::Array(theirs))) => {
            let base = match base {
                Some(Value::Array(base)) => base.as_slice(),
                _ => &[],
            };
            Some(Value::Array(merge_arrays(base, ours, theirs)))
        }
        _ => ours.cloned(),
    }
}

/// The `id` of a record, when it has a plain one.
fn record_id(value: &Value) -> Option<String> {
    let id = value.as_object()?.get("id")?;
    (id.is_string() || id.is_number()).then(|| id.to_string())
}

/// The ids of a list of records, when every item has one and no two share it.
fn record_ids(list: &[Value]) -> Option<Vec<String>> {
    let ids: Vec<String> = list.iter().map(record_id).collect::<Option<_>>()?;
    let unique: HashSet<&String> = ids.iter().collect();
    (unique.len() == ids.len()).then_some(ids)
}

fn merge_arrays(base: &[Value], ours: &[Value], theirs: &[Value]) -> Vec<Value> {
    if let (Some(base_ids), Some(our_ids), Some(their_ids)) =
        (record_ids(base), record_ids(ours), record_ids(theirs))
    {
        let base: HashMap<String, &Value> = base_ids.into_iter().zip(base).collect();
        let ours_by_id: HashMap<&String, &Value> = our_ids.iter().zip(ours).collect();
        let mut out = Vec::with_capacity(theirs.len().max(ours.len()));
        let mut seen = HashSet::new();
        for (id, their) in their_ids.iter().zip(theirs) {
            seen.insert(id.clone());
            let ours = ours_by_id.get(id).copied();
            if let Some(value) = merge(base.get(id).copied(), ours, Some(their)) {
                out.push(value);
            }
        }
        for (id, our) in our_ids.iter().zip(ours) {
            if seen.contains(id) {
                continue;
            }
            if let Some(value) = merge(base.get(id).copied(), Some(our), None) {
                out.push(value);
            }
        }
        return out;
    }
    // A set: what theirs holds, minus what ours removed, plus what ours added.
    let mut out: Vec<Value> = theirs
        .iter()
        .filter(|item| !(base.contains(item) && !ours.contains(item)))
        .cloned()
        .collect();
    for item in ours {
        if !base.contains(item) && !out.contains(item) {
            out.push(item.clone());
        }
    }
    out
}

/// Keeps a merged host book consistent: a recent connection or a favourite of a
/// record that is gone is dropped, a host has one recent record (the newest), and
/// recents are newest first.
pub(super) fn repair_hosts(book: &mut Value) {
    let Some(book) = book.as_object_mut() else {
        return;
    };
    let mut ids = HashSet::new();
    for list in ["hosts", "connections"] {
        if let Some(Value::Array(records)) = book.get(list) {
            ids.extend(records.iter().filter_map(record_id));
        }
    }
    if let Some(Value::Array(recents)) = book.get_mut("recents") {
        let when = |recent: &Value| {
            recent
                .get("last_connected_unix")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let host = |recent: &Value| recent.get("host").map(Value::to_string);
        let mut newest: HashMap<String, Value> = HashMap::new();
        for recent in recents.drain(..) {
            let Some(id) = host(&recent).filter(|id| ids.contains(id)) else {
                continue;
            };
            match newest.get(&id) {
                Some(kept) if when(kept) >= when(&recent) => {}
                _ => {
                    newest.insert(id, recent);
                }
            }
        }
        let mut kept: Vec<Value> = newest.into_values().collect();
        kept.sort_by(|a, b| when(b).cmp(&when(a)).then_with(|| host(a).cmp(&host(b))));
        *recents = kept;
    }
    if let Some(Value::Array(favorites)) = book.get_mut("favorites") {
        let mut marked = HashSet::new();
        favorites.retain(|favorite| {
            let id = favorite.to_string();
            ids.contains(&id) && marked.insert(id)
        });
    }
}

/// Keeps a merged group tree drawable: a path whose parent the other side removed
/// gets the parent back, and a path is listed once.
pub(super) fn repair_groups(book: &mut Value) {
    let Some(Value::Array(groups)) = book.get_mut("groups") else {
        return;
    };
    let mut tree = GroupBook::new();
    for path in groups.iter() {
        if let Ok(path) = serde_json::from_value::<GroupPath>(path.clone()) {
            let _ = tree.insert(&path);
        }
    }
    if let Ok(Value::Array(repaired)) = serde_json::to_value(tree.paths()) {
        *groups = repaired;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use shellune_core::HostConfig;

    use super::*;
    use crate::book::HostBook;

    fn host(name: &str) -> HostConfig {
        HostConfig::new(name, "10.0.0.1", 22, "root").expect("a valid host")
    }

    fn names(book: &HostBook) -> Vec<String> {
        let mut names: Vec<String> = book
            .hosts()
            .iter()
            .map(|host| host.name().to_owned())
            .collect();
        names.sort();
        names
    }

    fn state(hosts: HostBook) -> LocalState {
        LocalState {
            hosts,
            ..LocalState::default()
        }
    }

    fn touched_hosts() -> Touched {
        Touched {
            hosts: true,
            ..Touched::default()
        }
    }

    #[test]
    fn a_change_on_one_side_takes_that_side() {
        let base = json!({"a": 1, "list": [{"id": "x", "v": 1}]});
        let ours = json!({"a": 2, "list": [{"id": "x", "v": 1}]});
        let theirs = json!({"a": 1, "list": [{"id": "x", "v": 3}, {"id": "y", "v": 1}]});
        assert_eq!(
            merge(Some(&base), Some(&ours), Some(&theirs)),
            Some(json!({"a": 2, "list": [{"id": "x", "v": 3}, {"id": "y", "v": 1}]}))
        );
    }

    #[test]
    fn records_are_matched_by_id_and_deletions_on_either_side_hold() {
        let base = json!([{"id": 1, "v": "a"}, {"id": 2, "v": "b"}, {"id": 3, "v": "c"}]);
        // Ours removed 1 and edited 3; theirs removed 2 and added 4.
        let ours = json!([{"id": 2, "v": "b"}, {"id": 3, "v": "C"}, {"id": 5, "v": "e"}]);
        let theirs = json!([{"id": 1, "v": "a"}, {"id": 3, "v": "c"}, {"id": 4, "v": "d"}]);
        assert_eq!(
            merge(Some(&base), Some(&ours), Some(&theirs)),
            Some(json!([{"id": 3, "v": "C"}, {"id": 4, "v": "d"}, {"id": 5, "v": "e"}]))
        );
    }

    #[test]
    fn a_record_edited_here_survives_its_removal_there() {
        let base = json!([{"id": 1, "v": "a"}]);
        let ours = json!([{"id": 1, "v": "A"}]);
        let theirs = json!([]);
        assert_eq!(
            merge(Some(&base), Some(&ours), Some(&theirs)),
            Some(json!([{"id": 1, "v": "A"}]))
        );
    }

    #[test]
    fn plain_lists_merge_as_sets() {
        let base = json!(["a", "b"]);
        let ours = json!(["a", "c"]);
        let theirs = json!(["a", "b", "d"]);
        assert_eq!(
            merge(Some(&base), Some(&ours), Some(&theirs)),
            Some(json!(["a", "d", "c"]))
        );
    }

    #[test]
    fn a_favourite_made_here_lands_on_the_hosts_that_arrived() {
        let alpha = host("alpha");
        let alpha_id = alpha.id();
        let base = HostBook::from_hosts(vec![alpha]).expect("a book");
        let mut ours = base.clone();
        ours.set_favorite(alpha_id, true).expect("marked");
        ours.record_connection(alpha_id, 100).expect("recorded");
        let mut theirs = base.clone();
        theirs.insert(host("beta")).expect("added");

        let (merged, dropped) =
            rebase_state(&state(base), &state(ours), &state(theirs), touched_hosts());
        assert!(dropped.is_empty());
        assert_eq!(names(&merged.hosts), vec!["alpha", "beta"]);
        assert!(merged.hosts.is_favorite(alpha_id));
        assert_eq!(merged.hosts.recents().len(), 1);
        merged.hosts.ensure_valid().expect("a valid book");
    }

    #[test]
    fn marks_on_a_record_removed_there_are_dropped_with_it() {
        let alpha = host("alpha");
        let beta = host("beta");
        let beta_id = beta.id();
        let base = HostBook::from_hosts(vec![alpha, beta]).expect("a book");
        let mut ours = base.clone();
        ours.set_favorite(beta_id, true).expect("marked");
        ours.record_connection(beta_id, 100).expect("recorded");
        let mut theirs = base.clone();
        theirs.remove(beta_id).expect("removed there");

        let (merged, dropped) =
            rebase_state(&state(base), &state(ours), &state(theirs), touched_hosts());
        assert!(dropped.is_empty());
        assert_eq!(names(&merged.hosts), vec!["alpha"]);
        assert!(merged.hosts.favorite_hosts().is_empty());
        assert!(merged.hosts.recents().is_empty());
    }

    #[test]
    fn both_sides_connecting_keep_one_recent_record_the_newest() {
        let alpha = host("alpha");
        let alpha_id = alpha.id();
        let base = HostBook::from_hosts(vec![alpha]).expect("a book");
        let mut ours = base.clone();
        ours.record_connection(alpha_id, 100).expect("recorded");
        let mut theirs = base.clone();
        theirs.record_connection(alpha_id, 200).expect("recorded");
        theirs.insert(host("beta")).expect("added");
        let (merged, _) = rebase_state(&state(base), &state(ours), &state(theirs), touched_hosts());
        assert_eq!(merged.hosts.recents().len(), 1);
        merged.hosts.ensure_valid().expect("a valid book");
        assert_eq!(names(&merged.hosts), vec!["alpha", "beta"]);
    }

    #[test]
    fn a_group_whose_parent_was_removed_there_gets_it_back() {
        let path = |text: &str| GroupPath::parse(text).expect("a path");
        let mut base = GroupBook::new();
        base.insert(&path("Prod")).expect("added");
        let mut ours = base.clone();
        ours.insert(&path("Prod/Web")).expect("added");
        let theirs = GroupBook::new();
        let touched = Touched {
            groups: true,
            ..Touched::default()
        };
        let (merged, dropped) = rebase_state(
            &LocalState {
                groups: base,
                ..LocalState::default()
            },
            &LocalState {
                groups: ours,
                ..LocalState::default()
            },
            &LocalState {
                groups: theirs,
                ..LocalState::default()
            },
            touched,
        );
        assert!(dropped.is_empty());
        assert!(merged.groups.contains(&path("Prod/Web")));
        assert!(merged.groups.contains(&path("Prod")));
    }

    #[test]
    fn untouched_documents_stay_as_they_are_on_disk() {
        let base = LocalState::default();
        let mut ours = base.clone();
        let mut snippets = shellune_core::SnippetBook::new();
        snippets.create("list", "ls -la").expect("created");
        ours.snippets = snippets;
        let mut theirs = base.clone();
        theirs.hosts = HostBook::from_hosts(vec![host("alpha")]).expect("a book");
        let (merged, _) = rebase_state(&base, &ours, &theirs, Touched::default());
        assert_eq!(merged, theirs, "nothing was touched, so nothing is merged");
    }

    #[test]
    fn rebasing_writes_the_merged_files() {
        let dir = std::env::temp_dir().join(format!(
            "shellune-rebase-{}-{}",
            std::process::id(),
            super::super::now_unix()
        ));
        let data = LocalData::at(&dir);
        let alpha = host("alpha");
        let alpha_id = alpha.id();
        let base = HostBook::from_hosts(vec![alpha]).expect("a book");
        HostStore::at(&dir).save(&base).expect("written");
        let base_state = data.load().expect("reads");

        // Another writer adds beta on disk; the window marked alpha meanwhile.
        let mut theirs = base.clone();
        theirs.insert(host("beta")).expect("added");
        HostStore::at(&dir).save(&theirs).expect("written");
        let mut ours = base_state.clone();
        ours.hosts.set_favorite(alpha_id, true).expect("marked");

        let rebased = data
            .rebase(&base_state, &ours, touched_hosts())
            .expect("rebased");
        assert!(rebased.written);
        let on_disk = HostStore::at(&dir).load().expect("reads");
        assert_eq!(names(&on_disk), vec!["alpha", "beta"]);
        assert!(on_disk.is_favorite(alpha_id));
        assert_eq!(rebased.state.hosts, on_disk);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
