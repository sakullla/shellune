//! Copies of the data files taken before they are replaced.
//!
//! Every import, every pull from the cloud and every conflict decision first copies
//! the files it is about to change into `snapshots/<unix>-<reason>/` inside the data
//! directory. The copy is the restore point when a later write fails, and it is
//! what lets the side that lost a conflict be recovered by hand. Only the newest
//! [`SNAPSHOT_LIMIT`] are kept - counted apart for the snapshots a conflict decision
//! took (reasons starting with [`CONFLICT_REASON_PREFIX`]), so the routine pulls of an
//! automatic sync can never push the copy of the side a user chose to overwrite out
//! of the folder.

use std::path::{Path, PathBuf};

use super::PortableError;

/// Name of the directory, inside the data directory, that holds the snapshots.
pub const SNAPSHOT_DIRECTORY: &str = "snapshots";

/// How many snapshots are kept; older ones are removed when a new one is made.
pub const SNAPSHOT_LIMIT: usize = 10;

/// The start of the reason of a snapshot taken when a conflict was decided. These
/// are kept to their own [`SNAPSHOT_LIMIT`], apart from every other snapshot.
pub const CONFLICT_REASON_PREFIX: &str = "sync-use-";

/// One snapshot on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    path: PathBuf,
    created_unix: u64,
    reason: String,
    /// When the directory was last written, which orders two snapshots of one
    /// second that were taken for different reasons.
    modified: Option<std::time::SystemTime>,
}

impl Snapshot {
    /// The directory holding the copied files.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// When it was taken, in seconds since the Unix epoch.
    #[must_use]
    pub fn created_unix(&self) -> u64 {
        self.created_unix
    }

    /// The reason given when it was taken, as written in its name.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// The snapshot directory of one data directory.
#[derive(Clone, Debug)]
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    /// The snapshots kept beside the data files in `root`.
    #[must_use]
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory the snapshots live in.
    #[must_use]
    pub fn directory(&self) -> PathBuf {
        self.root.join(SNAPSHOT_DIRECTORY)
    }

    /// Writes a new snapshot holding `files` (name and bytes) and prunes the
    /// oldest ones beyond [`SNAPSHOT_LIMIT`].
    ///
    /// A file name may not contain a path separator. A partly written snapshot is
    /// removed, so a failure leaves no half copy behind.
    ///
    /// # Errors
    ///
    /// Returns [`PortableError::Snapshot`] when the directory or a file could not be
    /// written.
    pub fn create(
        &self,
        reason: &str,
        created_unix: u64,
        files: &[(&str, &[u8])],
    ) -> Result<PathBuf, PortableError> {
        let path = self.reserve(reason, created_unix)?;
        for (name, bytes) in files {
            let outcome = if name.contains(['/', '\\']) || name.is_empty() || *name == ".." {
                Err(format!("\"{name}\" is not a plain file name"))
            } else {
                std::fs::write(path.join(name), bytes).map_err(|error| error.to_string())
            };
            if let Err(reason) = outcome {
                let _ = std::fs::remove_dir_all(&path);
                return Err(PortableError::Snapshot { path, reason });
            }
        }
        self.prune(SNAPSHOT_LIMIT);
        Ok(path)
    }

    /// The snapshots on disk, oldest first.
    ///
    /// Directories whose name is not `<unix>-<reason>` are not snapshots this build
    /// made and are left out (and never removed).
    #[must_use]
    pub fn list(&self) -> Vec<Snapshot> {
        let Ok(entries) = std::fs::read_dir(self.directory()) else {
            return Vec::new();
        };
        let mut snapshots: Vec<Snapshot> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let (unix, reason) = name.split_once('-')?;
                let created_unix = unix.parse::<u64>().ok()?;
                Some(Snapshot {
                    path: entry.path(),
                    created_unix,
                    reason: reason.to_owned(),
                    modified: entry.metadata().and_then(|meta| meta.modified()).ok(),
                })
            })
            .collect();
        // Within one second the directory times order snapshots of different
        // reasons; for one reason a later snapshot also carries a longer collision
        // suffix (`-2`, ..., `-10`), so the length orders them before the text does.
        snapshots.sort_by(|left, right| {
            left.created_unix
                .cmp(&right.created_unix)
                .then_with(|| left.modified.cmp(&right.modified))
                .then_with(|| left.reason.len().cmp(&right.reason.len()))
                .then_with(|| left.reason.cmp(&right.reason))
        });
        snapshots
    }

    /// Removes the oldest snapshots so at most `keep` remain of the conflict
    /// decisions and at most `keep` of everything else. Best effort: a snapshot that
    /// cannot be removed is left for the next time.
    pub fn prune(&self, keep: usize) {
        let (conflicts, routine): (Vec<Snapshot>, Vec<Snapshot>) = self
            .list()
            .into_iter()
            .partition(|snapshot| snapshot.reason.starts_with(CONFLICT_REASON_PREFIX));
        for pool in [conflicts, routine] {
            let excess = pool.len().saturating_sub(keep);
            for snapshot in pool.into_iter().take(excess) {
                let _ = std::fs::remove_dir_all(snapshot.path());
            }
        }
    }

    /// Creates a fresh, empty snapshot directory with a name no other one has.
    fn reserve(&self, reason: &str, created_unix: u64) -> Result<PathBuf, PortableError> {
        let directory = self.directory();
        std::fs::create_dir_all(&directory).map_err(|error| PortableError::Snapshot {
            path: directory.clone(),
            reason: error.to_string(),
        })?;
        let base = format!("{created_unix}-{}", clean_reason(reason));
        // A later snapshot in the same second takes a higher suffix than any still
        // on disk, even after pruning freed a lower one, so it also sorts later.
        let taken = std::fs::read_dir(&directory)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name == base {
                            Some(1)
                        } else {
                            name.strip_prefix(&base)?
                                .strip_prefix('-')?
                                .parse::<u32>()
                                .ok()
                        }
                    })
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        for attempt in taken.saturating_add(1)..=taken.saturating_add(1000) {
            let name = if attempt == 1 {
                base.clone()
            } else {
                format!("{base}-{attempt}")
            };
            let path = directory.join(name);
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(PortableError::Snapshot {
                        path,
                        reason: error.to_string(),
                    });
                }
            }
        }
        Err(PortableError::Snapshot {
            path: directory.join(base),
            reason: "too many snapshots share this second and reason".to_owned(),
        })
    }
}

/// A reason as it may appear in a directory name: lower-case letters, digits and
/// dashes, at most 32 characters.
fn clean_reason(reason: &str) -> String {
    let mut cleaned: String = reason
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    while cleaned.contains("--") {
        cleaned = cleaned.replace("--", "-");
    }
    let cleaned = cleaned.trim_matches('-').to_owned();
    if cleaned.is_empty() {
        "change".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("shellune-snapshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_reason_becomes_a_safe_directory_name() {
        assert_eq!(clean_reason("Import backup"), "import-backup");
        assert_eq!(clean_reason("../../etc"), "etc");
        assert_eq!(clean_reason("   "), "change");
        assert_eq!(clean_reason("云端"), "change");
    }

    #[test]
    fn only_the_newest_snapshots_are_kept() {
        let root = scratch("prune");
        let store = SnapshotStore::at(&root);
        for second in 0..(SNAPSHOT_LIMIT as u64 + 3) {
            store
                .create("import", 1_000 + second, &[("hosts.json", b"{}")])
                .expect("a snapshot is written");
        }
        let kept = store.list();
        assert_eq!(kept.len(), SNAPSHOT_LIMIT);
        assert_eq!(kept[0].created_unix(), 1_003, "the three oldest went");
        assert_eq!(kept[0].reason(), "import");
        assert_eq!(
            std::fs::read(kept[0].path().join("hosts.json")).expect("copied"),
            b"{}"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn routine_snapshots_never_push_out_the_copy_a_conflict_decision_kept() {
        let root = scratch("conflict-pool");
        let store = SnapshotStore::at(&root);
        store
            .create("sync-use-remote", 100, &[("hosts.json", b"before")])
            .expect("the conflict snapshot is written");
        for second in 0..(SNAPSHOT_LIMIT as u64 * 2) {
            store
                .create("sync-pull", 200 + second, &[("hosts.json", b"{}")])
                .expect("a pull snapshot is written");
        }
        let kept = store.list();
        assert_eq!(kept.len(), SNAPSHOT_LIMIT + 1, "ten pulls and the decision");
        assert_eq!(kept[0].reason(), "sync-use-remote");
        assert_eq!(
            std::fs::read(kept[0].path().join("hosts.json")).expect("copied"),
            b"before"
        );
        // The decisions are limited among themselves too.
        for second in 0..(SNAPSHOT_LIMIT as u64 + 2) {
            store
                .create("sync-use-local", 1_000 + second, &[])
                .expect("a decision snapshot is written");
        }
        let decisions = store
            .list()
            .into_iter()
            .filter(|snapshot| snapshot.reason().starts_with(CONFLICT_REASON_PREFIX))
            .count();
        assert_eq!(decisions, SNAPSHOT_LIMIT);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn two_snapshots_in_one_second_do_not_collide() {
        let root = scratch("collide");
        let store = SnapshotStore::at(&root);
        let first = store.create("pull", 5, &[]).expect("first");
        let second = store.create("pull", 5, &[]).expect("second");
        assert_ne!(first, second);
        assert_eq!(store.list().len(), 2);
        for _ in 0..(SNAPSHOT_LIMIT + 1) {
            store.create("pull", 5, &[]).expect("another");
        }
        let kept: Vec<String> = store
            .list()
            .iter()
            .map(|snapshot| snapshot.reason().to_owned())
            .collect();
        assert_eq!(kept.len(), SNAPSHOT_LIMIT);
        assert_eq!(kept.first().map(String::as_str), Some("pull-4"), "{kept:?}");
        assert_eq!(kept.last().map(String::as_str), Some("pull-13"), "{kept:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_name_with_a_separator_is_refused_and_leaves_nothing() {
        let root = scratch("separator");
        let store = SnapshotStore::at(&root);
        let error = store
            .create("x", 9, &[("../hosts.json", b"{}")])
            .expect_err("refused");
        assert!(matches!(error, PortableError::Snapshot { .. }));
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
