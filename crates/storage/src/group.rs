//! The file the group tree lives in, and the changes that keep it and the hosts in
//! step (R18).
//!
//! A host names its group; the tree says which groups exist, empty ones included. The
//! two are separate documents, so a change that touches both — moving hosts, renaming
//! a group, deleting one — is written as a pair. The host file is written first
//! because it holds the records a user cannot recreate, and a failed second write is
//! rolled back to the bytes that were there before, so a failed change does not leave
//! half a new tree behind.
//!
//! Deleting a group never deletes a host: the caller names where the hosts go, and this
//! module moves them there before it removes the group. Nothing here connects, reads a
//! credential or changes a host's address, key path or sign-in choice.

use std::path::{Path, PathBuf};

use shellune_core::{GROUPS_FORMAT_VERSION, GroupBook, GroupBookError, GroupPath, HostId};

use crate::book::{HostBook, LegacyGroupLabel};
use crate::error::{BookError, PersistError, StorageError};
use crate::store::{self, HostStore};

/// Name of the group file inside the data directory.
pub const FILE_NAME: &str = "groups.json";

const LABEL: &str = "host groups";

/// The group tree at a known location.
#[derive(Clone, Debug)]
pub struct GroupStore {
    root: PathBuf,
}

impl GroupStore {
    /// A store inside the given directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A store beside the hosts, the workspace and the preferences.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(store::data_directory())
    }

    /// The directory the tree lives in.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The tree file.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(FILE_NAME)
    }

    /// Reads the tree.
    ///
    /// A missing file is a first run and yields an empty tree, which is exactly what a
    /// user who never created a group has. Reading starts nothing.
    ///
    /// # Errors
    ///
    /// Returns the read failure, the corruption (a repeated path, a path without its
    /// parent, a credential-shaped field) or the unsupported-version failure. The file
    /// is left as it was.
    pub fn load(&self) -> Result<GroupBook, PersistError> {
        store::load_versioned(
            &self.root,
            FILE_NAME,
            GROUPS_FORMAT_VERSION,
            LABEL,
            |book: &GroupBook| book.validate(),
        )
    }

    /// Writes the tree atomically.
    ///
    /// # Errors
    ///
    /// Returns a directory, write, replace or encode failure. The previous file stays
    /// as it was.
    pub fn save(&self, book: &GroupBook) -> Result<(), PersistError> {
        let bytes = serde_json::to_vec_pretty(book).map_err(|error| PersistError::Encode {
            label: LABEL,
            reason: error.to_string(),
        })?;
        store::save_versioned(&self.root, FILE_NAME, LABEL, &bytes)
    }
}

/// Why a group change did not happen.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    /// The host records refused the change.
    #[error("{0}")]
    Hosts(BookError),
    /// The tree refused the change.
    #[error("{0}")]
    Groups(GroupBookError),
    /// The host file could not be written; nothing else was attempted.
    #[error("{0}")]
    Storage(StorageError),
    /// The tree file could not be written.
    #[error("{0}")]
    Persist(PersistError),
    /// The hosts were written and the tree was not; the hosts were put back.
    #[error("the group change could not be saved: {failed}; the saved hosts were {restored}")]
    HalfWritten {
        /// Why the second write failed.
        failed: String,
        /// Whether the host file was returned to what it held before.
        restored: &'static str,
    },
}

impl From<BookError> for GroupError {
    fn from(error: BookError) -> Self {
        Self::Hosts(error)
    }
}

impl From<GroupBookError> for GroupError {
    fn from(error: GroupBookError) -> Self {
        Self::Groups(error)
    }
}

impl From<StorageError> for GroupError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<PersistError> for GroupError {
    fn from(error: PersistError) -> Self {
        Self::Persist(error)
    }
}

/// What a group change produced, once it is on disk.
#[derive(Clone, Debug)]
pub struct GroupUpdate {
    /// The hosts as they are now saved.
    pub hosts: HostBook,
    /// The tree as it is now saved.
    pub groups: GroupBook,
    /// How many hosts actually moved into another group.
    pub hosts_changed: usize,
    /// Hosts that still carry a saved label this build cannot place in the tree.
    ///
    /// They are reported rather than counted as ungrouped: the label stays visible,
    /// and a change that could not touch them says so instead of dropping them from
    /// its result.
    pub labels_without_group: Vec<LegacyGroupLabel>,
}

/// What deleting a group would touch, before anything is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupDeletion {
    /// How many hosts are in the group or inside it.
    pub hosts_moved: usize,
    /// The groups that would be removed, outermost first.
    pub groups_removed: Vec<GroupPath>,
    /// Where those hosts would go; `None` means out of every group.
    pub destination: Option<GroupPath>,
    /// Hosts that carry a saved label with no group path, so they are neither in the
    /// group nor moved with it. Listed for the confirmation instead of being left out
    /// of the count without a word.
    pub labels_without_group: Vec<LegacyGroupLabel>,
}

/// The host file and the group tree changed together.
///
/// Every method takes the documents it was given and returns new ones; the books on
/// disk only change when both writes succeed. That is what makes an invalid path, a
/// repeated name or a failed write a result the caller can report instead of a partly
/// applied tree.
#[derive(Clone, Debug)]
pub struct GroupService {
    hosts: HostStore,
    groups: GroupStore,
}

impl GroupService {
    /// A service over one data directory.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            hosts: HostStore::at(root.clone()),
            groups: GroupStore::at(root),
        }
    }

    /// A service at the per-user data directory of this platform.
    #[must_use]
    pub fn default_location() -> Self {
        Self::at(store::data_directory())
    }

    /// The host records this service changes.
    #[must_use]
    pub fn hosts(&self) -> &HostStore {
        &self.hosts
    }

    /// The tree this service changes.
    #[must_use]
    pub fn groups(&self) -> &GroupStore {
        &self.groups
    }

    /// Reads both documents as they are on disk.
    ///
    /// # Errors
    ///
    /// Returns the host or the tree failure; neither file is modified.
    pub fn load(&self) -> Result<(HostBook, GroupBook), GroupError> {
        Ok((self.hosts.load()?, self.groups.load()?))
    }

    /// Creates a group, and its parents when they are missing, and saves the tree.
    ///
    /// Hosts are not touched, so an empty group keeps existing for the next restart.
    ///
    /// # Errors
    ///
    /// Returns [`GroupBookError::AlreadyExists`] for a name that is taken, and the
    /// write failure when the tree could not be saved.
    pub fn create_group(
        &self,
        groups: &GroupBook,
        path: &GroupPath,
    ) -> Result<GroupBook, GroupError> {
        let mut next = groups.clone();
        next.insert(path)?;
        self.groups.save(&next)?;
        Ok(next)
    }

    /// Moves the given hosts into one group, or out of every group.
    ///
    /// A destination that is not saved yet is created with its parents, so moving a
    /// selection into a new path is one step rather than two.
    ///
    /// # Errors
    ///
    /// Returns the unknown host, the write failure, or [`GroupError::HalfWritten`] when
    /// the hosts were saved and the tree could not be.
    pub fn move_hosts(
        &self,
        hosts: &HostBook,
        groups: &GroupBook,
        ids: &[HostId],
        destination: Option<&GroupPath>,
    ) -> Result<GroupUpdate, GroupError> {
        let mut next_hosts = hosts.clone();
        let moved = next_hosts.reassign_group(ids, destination)?;
        let mut next_groups = groups.clone();
        if let Some(destination) = destination
            && !next_groups.contains(destination)
        {
            next_groups.insert(destination)?;
        }
        let labels_without_group = next_hosts.legacy_group_labels();
        self.commit(&next_hosts, &next_groups)?;
        Ok(GroupUpdate {
            hosts: next_hosts,
            groups: next_groups,
            hosts_changed: moved,
            labels_without_group,
        })
    }

    /// Renames a group, and moves every host inside it to the new path.
    ///
    /// # Errors
    ///
    /// Returns the unknown group, the taken name, a destination inside the group being
    /// renamed, or the write failure.
    pub fn rename_group(
        &self,
        hosts: &HostBook,
        groups: &GroupBook,
        from: &GroupPath,
        to: &GroupPath,
    ) -> Result<GroupUpdate, GroupError> {
        let mut next_groups = groups.clone();
        next_groups.rename(from, to)?;
        let mut next_hosts = hosts.clone();
        let moved = next_hosts.rename_group_paths(from, to)?;
        let labels_without_group = next_hosts.legacy_group_labels();
        self.commit(&next_hosts, &next_groups)?;
        Ok(GroupUpdate {
            hosts: next_hosts,
            groups: next_groups,
            hosts_changed: moved,
            labels_without_group,
        })
    }

    /// What deleting a group would touch.
    ///
    /// Nothing is written: the count and the destination are what a confirmation shows
    /// before the user decides.
    ///
    /// # Errors
    ///
    /// Returns [`GroupBookError::Unknown`] for a group that is not saved, and
    /// [`GroupBookError::NestedInsideItself`] when the destination lies inside the group
    /// being deleted.
    pub fn preview_deletion(
        &self,
        hosts: &HostBook,
        groups: &GroupBook,
        path: &GroupPath,
        destination: Option<&GroupPath>,
    ) -> Result<GroupDeletion, GroupError> {
        if !groups.contains(path) {
            return Err(GroupError::Groups(GroupBookError::Unknown {
                path: path.clone(),
            }));
        }
        if let Some(destination) = destination
            && destination.starts_with(path)
        {
            return Err(GroupError::Groups(GroupBookError::NestedInsideItself {
                from: path.clone(),
                into: destination.clone(),
            }));
        }
        let mut removed = vec![path.clone()];
        removed.extend(groups.descendants(path).into_iter().cloned());
        Ok(GroupDeletion {
            hosts_moved: hosts.hosts_under_group(path).len(),
            groups_removed: removed,
            destination: destination.cloned(),
            labels_without_group: hosts.legacy_group_labels(),
        })
    }

    /// Deletes a group, first moving its hosts to the destination.
    ///
    /// The destination is `None` for "out of every group", or another group, which is
    /// created with its parents when it is missing. The hosts keep their identifiers,
    /// addresses, keys and saved credentials; only their group changes.
    ///
    /// # Errors
    ///
    /// Returns the preview's reasons, or the write failure.
    pub fn delete_group(
        &self,
        hosts: &HostBook,
        groups: &GroupBook,
        path: &GroupPath,
        destination: Option<&GroupPath>,
    ) -> Result<(GroupUpdate, GroupDeletion), GroupError> {
        let preview = self.preview_deletion(hosts, groups, path, destination)?;
        let ids = hosts.hosts_under_group(path);
        let mut next_hosts = hosts.clone();
        let moved = next_hosts.reassign_group(&ids, destination)?;
        let mut next_groups = groups.clone();
        next_groups.remove(path)?;
        if let Some(destination) = destination
            && !next_groups.contains(destination)
        {
            next_groups.insert(destination)?;
        }
        let labels_without_group = next_hosts.legacy_group_labels();
        self.commit(&next_hosts, &next_groups)?;
        Ok((
            GroupUpdate {
                hosts_changed: moved,
                hosts: next_hosts,
                groups: next_groups,
                labels_without_group,
            },
            preview,
        ))
    }

    /// Writes the hosts and then the tree, putting the hosts back if the tree fails.
    ///
    /// The host file goes first because it holds the records a user cannot recreate
    /// from anything else; the tree can be rebuilt from the paths the hosts name. A
    /// failed second write answers with what actually happened instead of claiming the
    /// change was saved or silently leaving a tree that disagrees with the hosts.
    fn commit(&self, hosts: &HostBook, groups: &GroupBook) -> Result<(), GroupError> {
        let previous_hosts = std::fs::read(self.hosts.path()).ok();
        self.hosts.save(hosts)?;
        if let Err(error) = self.groups.save(groups) {
            let restored = restore(self.hosts.root(), crate::store::FILE_NAME, previous_hosts);
            return Err(GroupError::HalfWritten {
                failed: error.to_string(),
                restored: if restored { "restored" } else { "left changed" },
            });
        }
        Ok(())
    }
}

/// Puts a file back to the bytes it held, or removes it when it did not exist.
fn restore(root: &Path, name: &str, previous: Option<Vec<u8>>) -> bool {
    match previous {
        Some(bytes) => store::write_atomic_file(root, name, &bytes).is_ok(),
        None => {
            let path = root.join(name);
            std::fs::remove_file(&path).is_ok() || !path.exists()
        }
    }
}
