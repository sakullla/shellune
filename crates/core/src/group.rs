//! Hierarchical host groups: a validated path and the group tree that survives a
//! restart (R18).
//!
//! A group is a place a host can belong to, not a property of the connection. Nothing
//! here dials, authenticates or reads a credential: a path is text with rules, and the
//! tree is a list of paths.
//!
//! # Compatibility with the single label
//!
//! Before hierarchies existed a host carried one optional label. That label is still
//! stored exactly as it was written, in its own field, and reads as a path of one
//! segment ([`HostConfig::group_path`]). A path this build assigns is stored in a
//! separate field, so an old file keeps its shape and an old label that happens to
//! contain the separator stays one segment instead of silently becoming two.
//!
//! # The canonical form
//!
//! A path is written as its segments joined by `/`. Because a segment may itself
//! contain `/` (an old label can, and it must stay readable), a separator inside a
//! segment is escaped as `\/` and a backslash as `\\`. Parsing is therefore exact:
//! every path this module writes reads back as the same segments.
//!
//! # Limits
//!
//! Depth and segment length are bounded. The bounds keep the tree drawable in a
//! narrow window and one record from making the file unbounded; a path over a limit is
//! refused with its reason rather than quietly shortened, because a shortened path is
//! not the path the user typed.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The separator between two segments of a group path.
pub const GROUP_SEPARATOR: char = '/';

/// The deepest path this build accepts, counting the top segment as one.
///
/// The group tree is drawn inside the window's own column, so a deeper path would run
/// out of room for the names rather than adding anything the user can read.
pub const MAX_GROUP_DEPTH: usize = 8;

/// The longest single group name, in characters.
pub const MAX_GROUP_SEGMENT_CHARS: usize = 64;

/// The format this build writes for the group file.
pub const GROUPS_FORMAT_VERSION: u32 = 1;

/// Why a group path was refused.
///
/// Every variant names what to change, because "invalid group" does not tell a user
/// anything they can act on.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GroupPathError {
    /// The whole path is empty or only spaces.
    #[error("a group name cannot be empty")]
    Empty,
    /// One segment between two separators is empty.
    #[error("the group path has an empty name at position {index}; remove the repeated separator")]
    EmptySegment {
        /// Zero-based position of the segment in the path.
        index: usize,
    },
    /// One segment is longer than [`MAX_GROUP_SEGMENT_CHARS`].
    #[error("the group name at position {index} has {chars} characters, more than {max}")]
    SegmentTooLong {
        /// Zero-based position of the segment in the path.
        index: usize,
        /// How long the segment is.
        chars: usize,
        /// The limit that applies.
        max: usize,
    },
    /// The path has more segments than [`MAX_GROUP_DEPTH`].
    #[error("the group path has {depth} levels, more than the {max} that can be shown")]
    TooDeep {
        /// How many levels the path has.
        depth: usize,
        /// The limit that applies.
        max: usize,
    },
    /// A segment contains a control character.
    #[error("the group name at position {index} contains a control character")]
    ControlCharacter {
        /// Zero-based position of the segment in the path.
        index: usize,
    },
    /// A segment is `.` or `..`, which would read as navigation rather than a name.
    #[error("the group name at position {index} is reserved; choose another name")]
    ReservedSegment {
        /// Zero-based position of the segment in the path.
        index: usize,
    },
    /// The text ends with a backslash that escapes nothing.
    #[error("the group path ends with a backslash at position {index}; remove it or escape it")]
    DanglingEscape {
        /// Character position of the trailing backslash.
        index: usize,
    },
}

/// A validated position in the group tree.
///
/// A path always has at least one segment, and no segment is empty, longer than
/// [`MAX_GROUP_SEGMENT_CHARS`] characters, or a control character. Equality and order
/// are by segments, so `Prod` sorts before `Prod/Web`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupPath {
    segments: Vec<String>,
}

impl GroupPath {
    /// Parses the canonical text of a path.
    ///
    /// # Errors
    ///
    /// Returns the first problem found: an empty path, an empty segment, a segment that
    /// is too long, a control character, a reserved name, or a trailing escape.
    pub fn parse(text: &str) -> Result<Self, GroupPathError> {
        if text.trim().is_empty() {
            return Err(GroupPathError::Empty);
        }
        let mut segments: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut chars = text.char_indices();
        while let Some((index, ch)) = chars.next() {
            match ch {
                '\\' => match chars.next() {
                    Some((_, escaped)) => current.push(escaped),
                    None => return Err(GroupPathError::DanglingEscape { index }),
                },
                GROUP_SEPARATOR => segments.push(std::mem::take(&mut current)),
                _ => current.push(ch),
            }
        }
        segments.push(current);
        Self::from_segments(segments)
    }

    /// Reads the single label an earlier build stored, as a path of one segment.
    ///
    /// The text is taken as it is, separators included: a label of `Prod/Web` was one
    /// group when it was saved and stays one group. Surrounding spaces are trimmed,
    /// exactly as the label was trimmed when it was stored.
    ///
    /// # Errors
    ///
    /// Returns [`GroupPathError::Empty`] for a blank label, or the limit that the label
    /// breaks. A label with a control character has no path form.
    pub fn from_label(label: &str) -> Result<Self, GroupPathError> {
        Self::from_segments(vec![label.to_owned()])
    }

    /// Builds a path from its segments, checking each one.
    ///
    /// # Errors
    ///
    /// Returns the first problem found, including a path with no segments.
    pub fn from_segments(
        segments: impl IntoIterator<Item = String>,
    ) -> Result<Self, GroupPathError> {
        let segments: Vec<String> = segments.into_iter().collect();
        if segments.is_empty() {
            return Err(GroupPathError::Empty);
        }
        if segments.len() > MAX_GROUP_DEPTH {
            return Err(GroupPathError::TooDeep {
                depth: segments.len(),
                max: MAX_GROUP_DEPTH,
            });
        }
        let mut checked: Vec<String> = Vec::with_capacity(segments.len());
        for (index, raw) in segments.iter().enumerate() {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(GroupPathError::EmptySegment { index });
            }
            if trimmed.chars().any(char::is_control) {
                return Err(GroupPathError::ControlCharacter { index });
            }
            if trimmed == "." || trimmed == ".." {
                return Err(GroupPathError::ReservedSegment { index });
            }
            let chars = trimmed.chars().count();
            if chars > MAX_GROUP_SEGMENT_CHARS {
                return Err(GroupPathError::SegmentTooLong {
                    index,
                    chars,
                    max: MAX_GROUP_SEGMENT_CHARS,
                });
            }
            checked.push(trimmed.to_owned());
        }
        Ok(Self { segments: checked })
    }

    /// The segments, outermost first.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// The innermost segment, which is the name shown beside the hosts.
    #[must_use]
    pub fn leaf(&self) -> &str {
        self.segments.last().map_or("", String::as_str)
    }

    /// How many levels the path has; never zero.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.segments.len()
    }

    /// The enclosing group, or `None` for a top-level group.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        match self.segments.len() {
            0 | 1 => None,
            len => Some(Self {
                segments: self.segments[..len - 1].to_vec(),
            }),
        }
    }

    /// The same group with one more segment inside it.
    ///
    /// # Errors
    ///
    /// Returns the reason the added segment was refused, including a path that would
    /// become too deep.
    pub fn child(&self, segment: &str) -> Result<Self, GroupPathError> {
        let mut segments = self.segments.clone();
        segments.push(segment.to_owned());
        Self::from_segments(segments)
    }

    /// Whether this path is `other` or lies inside it.
    #[must_use]
    pub fn starts_with(&self, other: &Self) -> bool {
        self.segments.len() >= other.segments.len()
            && self.segments[..other.segments.len()] == other.segments[..]
    }

    /// This path moved from under `from` to under `to`.
    ///
    /// `None` when this path is not inside `from`, or when the moved path would be
    /// deeper than [`MAX_GROUP_DEPTH`].
    #[must_use]
    pub fn rebase(&self, from: &Self, to: &Self) -> Option<Self> {
        if !self.starts_with(from) {
            return None;
        }
        let rest = &self.segments[from.segments.len()..];
        if to.segments.len() + rest.len() > MAX_GROUP_DEPTH {
            return None;
        }
        let mut segments = to.segments.clone();
        segments.extend_from_slice(rest);
        Some(Self { segments })
    }

    /// The canonical text of this path, the form that is stored.
    #[must_use]
    pub fn to_canonical(&self) -> String {
        let mut text = String::new();
        for (index, segment) in self.segments.iter().enumerate() {
            if index > 0 {
                text.push(GROUP_SEPARATOR);
            }
            for ch in segment.chars() {
                if ch == '\\' || ch == GROUP_SEPARATOR {
                    text.push('\\');
                }
                text.push(ch);
            }
        }
        text
    }
}

impl fmt::Display for GroupPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_canonical())
    }
}

impl Serialize for GroupPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_canonical())
    }
}

impl<'de> Deserialize<'de> for GroupPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(D::Error::custom)
    }
}

/// Why the group tree was not changed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GroupBookError {
    /// The group is already in the tree.
    #[error("the group {path} already exists; choose another name")]
    AlreadyExists {
        /// The path that was already there.
        path: GroupPath,
    },
    /// The group is not in the tree.
    #[error("the group {path} is not saved; reload the group list")]
    Unknown {
        /// The path that was not found.
        path: GroupPath,
    },
    /// A group cannot be moved inside itself.
    #[error("the group {from} cannot be moved inside {into}, which is already within it")]
    NestedInsideItself {
        /// The group being renamed.
        from: GroupPath,
        /// The destination that lies inside it.
        into: GroupPath,
    },
    /// A path this build would refuse was offered.
    #[error("{0}")]
    InvalidPath(GroupPathError),
    /// A stored tree breaks the invariants of a tree.
    #[error("the saved groups are inconsistent: {reason}")]
    Inconsistent {
        /// What is wrong with the stored paths.
        reason: String,
    },
}

impl From<GroupPathError> for GroupBookError {
    fn from(error: GroupPathError) -> Self {
        Self::InvalidPath(error)
    }
}

/// The groups the user created, including the empty ones.
///
/// Hosts are not stored here: a host names its path, and this tree says which groups
/// exist even when no host is in them. That is what lets an empty group survive a
/// restart. Every stored path has its ancestors stored beside it, so the tree is
/// always drawable without inventing a parent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "GroupBookWire")]
pub struct GroupBook {
    version: u32,
    groups: Vec<GroupPath>,
}

/// The stored shape of the group tree.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupBookWire {
    version: u32,
    #[serde(default)]
    groups: Vec<GroupPath>,
}

impl TryFrom<GroupBookWire> for GroupBook {
    type Error = GroupBookError;

    fn try_from(wire: GroupBookWire) -> Result<Self, Self::Error> {
        let book = Self {
            version: wire.version,
            groups: wire.groups,
        };
        book.validate()
            .map_err(|reason| GroupBookError::Inconsistent { reason })?;
        Ok(book)
    }
}

impl Default for GroupBook {
    fn default() -> Self {
        Self {
            version: GROUPS_FORMAT_VERSION,
            groups: Vec::new(),
        }
    }
}

impl GroupBook {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A tree seeded with paths, adding any missing ancestors.
    ///
    /// # Errors
    ///
    /// Returns [`GroupBookError::AlreadyExists`] for a repeated path, so a fixture or an
    /// import cannot quietly list one group twice.
    pub fn from_paths(paths: impl IntoIterator<Item = GroupPath>) -> Result<Self, GroupBookError> {
        let mut book = Self::new();
        for path in paths {
            book.insert(&path)?;
        }
        Ok(book)
    }

    /// The format version the tree was read from.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// Every stored path, in the order the groups were created.
    #[must_use]
    pub fn paths(&self) -> &[GroupPath] {
        &self.groups
    }

    /// How many groups are stored, empty groups included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    /// Whether no group is stored at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Whether this exact group is stored.
    #[must_use]
    pub fn contains(&self, path: &GroupPath) -> bool {
        self.groups.iter().any(|saved| saved == path)
    }

    /// Adds a group, and any missing ancestors, at the end of its level.
    ///
    /// # Errors
    ///
    /// Returns [`GroupBookError::AlreadyExists`] when the group itself is already
    /// stored. Adding `Prod/Web` therefore creates `Prod` only when it is missing.
    pub fn insert(&mut self, path: &GroupPath) -> Result<(), GroupBookError> {
        if self.contains(path) {
            return Err(GroupBookError::AlreadyExists { path: path.clone() });
        }
        let mut missing: Vec<GroupPath> = Vec::new();
        let mut current = Some(path.clone());
        while let Some(candidate) = current {
            if self.contains(&candidate) {
                break;
            }
            current = candidate.parent();
            missing.push(candidate);
        }
        self.groups.extend(missing.into_iter().rev());
        Ok(())
    }

    /// The groups directly inside `parent`, or the top-level groups for `None`, in
    /// creation order.
    #[must_use]
    pub fn children(&self, parent: Option<&GroupPath>) -> Vec<&GroupPath> {
        let depth = parent.map_or(1, |parent| parent.depth() + 1);
        self.groups
            .iter()
            .filter(|path| path.depth() == depth)
            .filter(|path| parent.is_none_or(|parent| path.starts_with(parent)))
            .collect()
    }

    /// Every group inside `path`, not counting `path` itself.
    #[must_use]
    pub fn descendants(&self, path: &GroupPath) -> Vec<&GroupPath> {
        self.groups
            .iter()
            .filter(|saved| saved.starts_with(path) && *saved != path)
            .collect()
    }

    /// Renames a group and everything inside it.
    ///
    /// Returns the paths that changed, as `(old, new)` pairs in stored order, so the
    /// caller can move the hosts that named the old paths. A destination below a parent
    /// that was never created gets that parent, because a stored path without its
    /// ancestors is a tree this build would refuse to read back.
    ///
    /// The whole rewrite is staged first and checked against the same invariants a
    /// file has to pass when it is loaded, so a rename either happens completely or
    /// leaves the tree exactly as it was: a row that could not be moved does not leave
    /// a half-renamed tree behind.
    ///
    /// # Errors
    ///
    /// Returns [`GroupBookError::Unknown`] when the group is not stored,
    /// [`GroupBookError::AlreadyExists`] when the new name is taken and
    /// [`GroupBookError::NestedInsideItself`] when the destination lies inside the
    /// group being renamed; a rename never produces a group with two parents. A move
    /// that would make a path too deep or that would collide with a saved group is
    /// refused before anything is changed.
    pub fn rename(
        &mut self,
        from: &GroupPath,
        to: &GroupPath,
    ) -> Result<Vec<(GroupPath, GroupPath)>, GroupBookError> {
        if !self.contains(from) {
            return Err(GroupBookError::Unknown { path: from.clone() });
        }
        if self.contains(to) {
            return Err(GroupBookError::AlreadyExists { path: to.clone() });
        }
        if to.starts_with(from) {
            return Err(GroupBookError::NestedInsideItself {
                from: from.clone(),
                into: to.clone(),
            });
        }

        // Stage every rewritten row before one is assigned: the tree on disk must never
        // be a partly renamed one.
        let mut staged = self.groups.clone();
        let mut changes: Vec<(GroupPath, GroupPath)> = Vec::new();
        for (index, old) in self.groups.iter().enumerate() {
            if !old.starts_with(from) {
                continue;
            }
            let Some(new) = old.rebase(from, to) else {
                return Err(GroupBookError::InvalidPath(GroupPathError::TooDeep {
                    depth: to.depth() + old.depth() - from.depth(),
                    max: MAX_GROUP_DEPTH,
                }));
            };
            // The destination path may already be taken by a group that is not part of
            // the rename; two groups with one name have no single place in the tree.
            if self.groups.contains(&new) && !new.starts_with(from) {
                return Err(GroupBookError::AlreadyExists { path: new });
            }
            staged[index] = new.clone();
            changes.push((old.clone(), new));
        }

        // A destination such as `Lab/New` may name a parent that was never created;
        // storing the child without it produces a file that cannot be loaded again.
        let mut missing: Vec<GroupPath> = Vec::new();
        let mut parent = to.parent();
        while let Some(candidate) = parent {
            if staged.contains(&candidate) {
                break;
            }
            parent = candidate.parent();
            missing.push(candidate);
        }
        staged.extend(missing.into_iter().rev());

        let staged_book = Self {
            version: self.version,
            groups: staged,
        };
        staged_book
            .validate()
            .map_err(|reason| GroupBookError::Inconsistent { reason })?;
        self.groups = staged_book.groups;
        Ok(changes)
    }

    /// Removes a group and everything inside it.
    ///
    /// Returns the removed paths. Hosts are not touched: they live in the host book,
    /// and the caller decides where they go before calling this.
    ///
    /// # Errors
    ///
    /// Returns [`GroupBookError::Unknown`] when the group is not stored.
    pub fn remove(&mut self, path: &GroupPath) -> Result<Vec<GroupPath>, GroupBookError> {
        if !self.contains(path) {
            return Err(GroupBookError::Unknown { path: path.clone() });
        }
        let mut removed = Vec::new();
        self.groups.retain(|saved| {
            if saved.starts_with(path) {
                removed.push(saved.clone());
                false
            } else {
                true
            }
        });
        Ok(removed)
    }

    /// Checks the invariants a stored tree must satisfy.
    ///
    /// # Errors
    ///
    /// Returns a sentence naming the repeated path or the missing ancestor.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen: Vec<&GroupPath> = Vec::with_capacity(self.groups.len());
        for path in &self.groups {
            if seen.contains(&path) {
                return Err(format!(
                    "the group {path} is listed twice, so the tree has no single place for it"
                ));
            }
            seen.push(path);
        }
        for path in &self.groups {
            let mut parent = path.parent();
            while let Some(candidate) = parent {
                if !self.contains(&candidate) {
                    return Err(format!(
                        "the group {path} is listed without its parent {candidate}"
                    ));
                }
                parent = candidate.parent();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> GroupPath {
        GroupPath::parse(text).expect("a valid test path")
    }

    #[test]
    fn a_path_keeps_its_segments_in_order() {
        let prod = path("Prod");
        let web = path("Prod/Web");
        assert_eq!(prod.depth(), 1);
        assert_eq!(web.segments(), ["Prod", "Web"]);
        assert_eq!(web.leaf(), "Web");
        assert_eq!(web.parent(), Some(prod.clone()));
        assert!(web.starts_with(&prod), "a child is inside its parent");
        assert!(!prod.starts_with(&web), "a parent is not inside its child");
        assert_eq!(
            web.rebase(&prod, &path("Staging")),
            Some(path("Staging/Web"))
        );
        assert_eq!(
            web.rebase(&path("Lab"), &prod),
            None,
            "a rename only moves what is inside it"
        );
        assert_eq!(web.to_string(), "Prod/Web");
        assert_eq!(GroupPath::parse(&web.to_string()), Ok(web));
    }

    #[test]
    fn a_label_that_contains_the_separator_stays_one_segment() {
        // An earlier build stored one label. The separator was an ordinary character in
        // it, so it must not become two levels when the hierarchy arrives.
        let label = GroupPath::from_label("Prod/Web").expect("a label has a path form");
        assert_eq!(label.depth(), 1);
        assert_eq!(label.segments(), ["Prod/Web"]);
        let text = label.to_canonical();
        assert_eq!(
            text, "Prod\\/Web",
            "the separator inside the name is escaped"
        );
        assert_eq!(GroupPath::parse(&text), Ok(label));
    }

    #[test]
    fn a_path_that_breaks_a_limit_is_refused_with_its_reason() {
        assert_eq!(GroupPath::parse(""), Err(GroupPathError::Empty));
        assert_eq!(GroupPath::parse("   "), Err(GroupPathError::Empty));
        assert_eq!(
            GroupPath::parse("Prod//Web"),
            Err(GroupPathError::EmptySegment { index: 1 })
        );
        assert_eq!(
            GroupPath::parse("Prod/./Web"),
            Err(GroupPathError::ReservedSegment { index: 1 })
        );
        assert_eq!(
            GroupPath::parse("Prod\u{7}Web"),
            Err(GroupPathError::ControlCharacter { index: 0 })
        );
        assert_eq!(
            GroupPath::parse("Prod\\"),
            Err(GroupPathError::DanglingEscape { index: 4 })
        );
        let long = "n".repeat(MAX_GROUP_SEGMENT_CHARS + 1);
        assert_eq!(
            GroupPath::parse(&long),
            Err(GroupPathError::SegmentTooLong {
                index: 0,
                chars: MAX_GROUP_SEGMENT_CHARS + 1,
                max: MAX_GROUP_SEGMENT_CHARS,
            })
        );
        let deep = ["n"; MAX_GROUP_DEPTH + 1].join("/");
        assert_eq!(
            GroupPath::parse(&deep),
            Err(GroupPathError::TooDeep {
                depth: MAX_GROUP_DEPTH + 1,
                max: MAX_GROUP_DEPTH,
            })
        );
    }

    #[test]
    fn a_tree_adds_the_parents_of_a_new_group_and_stops_at_a_repeated_name() {
        let mut book = GroupBook::new();
        book.insert(&path("Prod/Web/Eu")).expect("the group is new");
        assert_eq!(
            book.paths(),
            [path("Prod"), path("Prod/Web"), path("Prod/Web/Eu")],
            "a group that is drawn needs its parents stored beside it"
        );
        assert_eq!(book.children(None), [&path("Prod")]);
        assert_eq!(book.children(Some(&path("Prod"))), [&path("Prod/Web")]);
        assert_eq!(book.descendants(&path("Prod")).len(), 2);
        assert_eq!(book.validate(), Ok(()));

        assert_eq!(
            book.insert(&path("Prod/Web/Eu")),
            Err(GroupBookError::AlreadyExists {
                path: path("Prod/Web/Eu"),
            })
        );
        assert_eq!(book.len(), 3, "a refused name leaves the tree as it was");
    }

    #[test]
    fn renaming_a_group_renames_what_is_inside_it() {
        let mut book = GroupBook::from_paths([
            path("Prod"),
            path("Prod/Web"),
            path("Prod/Web/Eu"),
            path("Lab"),
        ])
        .expect("the tree is valid");

        let changes = book
            .rename(&path("Prod"), &path("Staging"))
            .expect("the rename is valid");
        assert_eq!(changes.len(), 3, "the group and the two below it moved");
        assert_eq!(
            book.paths(),
            [
                path("Staging"),
                path("Staging/Web"),
                path("Staging/Web/Eu"),
                path("Lab")
            ]
        );

        assert_eq!(
            book.rename(&path("Staging"), &path("Staging/Web/Inner")),
            Err(GroupBookError::NestedInsideItself {
                from: path("Staging"),
                into: path("Staging/Web/Inner"),
            })
        );
        assert_eq!(
            book.rename(&path("Lab"), &path("Staging")),
            Err(GroupBookError::AlreadyExists {
                path: path("Staging"),
            })
        );
        assert_eq!(
            book.rename(&path("Gone"), &path("Elsewhere")),
            Err(GroupBookError::Unknown { path: path("Gone") })
        );
    }

    #[test]
    fn removing_a_group_removes_what_is_inside_it_and_says_which() {
        let mut book =
            GroupBook::from_paths([path("Prod/Web"), path("Lab")]).expect("the tree is valid");
        let removed = book.remove(&path("Prod")).expect("the group is saved");
        assert_eq!(removed, [path("Prod"), path("Prod/Web")]);
        assert_eq!(book.paths(), [path("Lab")]);
        assert_eq!(
            book.remove(&path("Prod")),
            Err(GroupBookError::Unknown { path: path("Prod") })
        );
    }

    #[test]
    fn a_rename_adds_the_parents_of_its_destination() {
        // "Staging" was never created: a tree that stored "Staging/New" without it
        // would be a file this build refuses to read back.
        let mut book =
            GroupBook::from_paths([path("Prod"), path("Prod/Web")]).expect("the tree is valid");
        let changes = book
            .rename(&path("Prod"), &path("Staging/New"))
            .expect("the rename is valid");
        assert_eq!(
            changes,
            [
                (path("Prod"), path("Staging/New")),
                (path("Prod/Web"), path("Staging/New/Web"))
            ]
        );
        assert_eq!(
            book.paths(),
            [
                path("Staging/New"),
                path("Staging/New/Web"),
                path("Staging")
            ],
            "the destination's parent is stored with it, at the end like any new group"
        );
        assert_eq!(book.validate(), Ok(()), "the tree can be read back");
    }

    #[test]
    fn a_rename_that_cannot_finish_leaves_the_tree_untouched() {
        let mut book =
            GroupBook::from_paths([path("Prod"), path("Prod/Web")]).expect("the tree is valid");
        let before = book.clone();
        let deep = path(&["n"; MAX_GROUP_DEPTH].join("/"));
        assert_eq!(
            book.rename(&path("Prod"), &deep),
            Err(GroupBookError::InvalidPath(GroupPathError::TooDeep {
                depth: MAX_GROUP_DEPTH + 1,
                max: MAX_GROUP_DEPTH,
            })),
            "the child cannot follow its group that deep"
        );
        assert_eq!(
            book, before,
            "a rename that stops half way leaves nothing changed"
        );
    }
}
