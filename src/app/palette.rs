//! The command palette: one searchable list of everything the window can do.
//!
//! A terminal user spends most of the time with the keyboard on a shell; going to the
//! sidebar to switch a session, start another shell or connect to a saved host breaks
//! that. The palette gathers those destinations behind one shortcut, the way other
//! terminals offer a profile or connection selector: open sessions, local shells,
//! saved hosts, the five pages and the window settings, filtered as the user types.
//!
//! This module is render-independent: it holds the entries and the ranking, so the
//! matching rules can be tested without a window.

use shellune_core::{HostId, SessionId};

use crate::app::icons::Icon;
use crate::app::nav::Destination;

/// What choosing an entry does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Shows the terminal of an open session.
    Switch(SessionId),
    /// Opens one of the detected shells in a new tab.
    Shell(usize),
    /// Opens the connection form of a saved host.
    Connect(HostId),
    /// Goes to one page.
    GoTo(Destination),
    /// Opens the form for a new saved host.
    NewHost,
    /// Opens the SSH configuration import.
    ImportSshConfig,
    /// Connects with an address that is not saved as a host first.
    QuickConnect,
    /// Closes the session on screen.
    CloseTab(SessionId),
    /// Gives the tab of an open session a name of its own.
    RenameTab(SessionId),
    /// Drops the name of an open session's tab and shows its automatic title again.
    RestoreTabTitle(SessionId),
    /// Steps the distinguishing colour of an open session's tab.
    CycleTabAccent(SessionId),
    /// Clears the distinguishing colour of an open session's tab.
    ClearTabAccent(SessionId),
    /// Switches the interface language.
    ToggleLanguage,
    /// Steps the interface text size.
    CycleTextSize,
    /// Steps the theme.
    CycleTheme,
    /// Switches short interface motion off or on.
    CycleMotion,
}

/// The section an entry is listed under when nothing is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// Open sessions.
    Sessions,
    /// Hosts that were reached, newest first.
    Recent,
    /// Shells that can be started.
    Shells,
    /// Saved hosts.
    Hosts,
    /// The pages of the window.
    Navigate,
    /// Window actions and settings.
    Actions,
}

/// One line of the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// What choosing it does.
    pub command: Command,
    /// Where it is listed.
    pub group: Group,
    /// The name the user searches for.
    pub title: String,
    /// A second, quieter line: a program, an address, a state.
    pub detail: String,
    /// The shortcut that does the same thing, when there is one.
    pub shortcut: Option<String>,
    /// The drawing in front of it.
    pub icon: Icon,
    /// Whether this is the session already on screen.
    pub current: bool,
}

/// The most entries a search lists; the rest are one more letter away.
pub const MAX_RESULTS: usize = 60;

/// The entries that match `query`, best first.
///
/// With nothing typed every entry is listed in its group order, so the palette reads
/// as a menu. Otherwise each word of the query has to be found in the title (letters
/// in order, not necessarily adjacent, once the word has three letters) or in the
/// detail (as typed), and the entries
/// are ordered by how well the title matched; ties keep the group order.
#[must_use]
pub fn rank(entries: &[Entry], query: &str) -> Vec<Entry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        let mut all = entries.to_vec();
        all.sort_by_key(|entry| entry.group);
        all.truncate(MAX_RESULTS);
        return all;
    }
    let mut scored: Vec<(u32, &Entry)> = entries
        .iter()
        .filter_map(|entry| score(entry, &words).map(|score| (score, entry)))
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.group.cmp(&b.1.group)));
    scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, entry)| entry.clone())
        .collect()
}

/// How well an entry matches every word, or `None` when one word is missing.
fn score(entry: &Entry, words: &[String]) -> Option<u32> {
    let title = entry.title.to_lowercase();
    let detail = entry.detail.to_lowercase();
    let mut total = 0;
    for word in words {
        total += if title.starts_with(word.as_str()) {
            8
        } else if title
            .split(|c: char| !c.is_alphanumeric())
            .any(|part| part.starts_with(word.as_str()))
        {
            6
        } else if title.contains(word.as_str()) {
            4
        } else if detail.contains(word.as_str()) {
            2
        } else if word.chars().count() >= 3 && is_subsequence(word, &title) {
            1
        } else {
            return None;
        };
    }
    Some(total)
}

/// Whether the letters of `needle` appear in `haystack` in the same order.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut letters = haystack.chars();
    needle
        .chars()
        .all(|wanted| letters.by_ref().any(|letter| letter == wanted))
}

/// The selection after a step of `delta`, wrapping at both ends.
#[must_use]
pub fn step(selected: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let len = isize::try_from(len).unwrap_or(isize::MAX);
    let current = isize::try_from(selected).unwrap_or(0).min(len - 1);
    usize::try_from((current + delta).rem_euclid(len)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, detail: &str, group: Group) -> Entry {
        Entry {
            command: Command::ToggleLanguage,
            group,
            title: title.to_owned(),
            detail: detail.to_owned(),
            shortcut: None,
            icon: Icon::Terminal,
            current: false,
        }
    }

    #[test]
    fn an_empty_query_lists_everything_in_group_order() {
        let entries = [
            entry("Language", "", Group::Actions),
            entry("Bash", "bash", Group::Shells),
            entry("prod", "root@10.0.0.1", Group::Hosts),
        ];
        let titles: Vec<String> = rank(&entries, "  ")
            .into_iter()
            .map(|entry| entry.title)
            .collect();
        assert_eq!(titles, ["Bash", "prod", "Language"]);
    }

    #[test]
    fn a_title_prefix_beats_a_detail_match() {
        let entries = [
            entry("staging", "deploy@prod.example", Group::Hosts),
            entry("prod", "root@10.0.0.1", Group::Hosts),
        ];
        let ranked = rank(&entries, "prod");
        assert_eq!(ranked[0].title, "prod");
        assert_eq!(ranked.len(), 2, "the address match is still listed");
    }

    #[test]
    fn every_word_has_to_match() {
        let entries = [
            entry("web 1", "root@web1", Group::Hosts),
            entry("db 1", "root@db1", Group::Hosts),
        ];
        let ranked = rank(&entries, "web root");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].title, "web 1");
        assert!(rank(&entries, "web nothing").is_empty());
    }

    #[test]
    fn letters_in_order_match_loosely() {
        let entries = [entry("PowerShell", "", Group::Shells)];
        assert_eq!(rank(&entries, "pwsh").len(), 1);
        assert!(rank(&entries, "hsp").is_empty());
    }

    #[test]
    fn matching_ignores_case() {
        let entries = [entry("Bash", "/usr/bin/bash", Group::Shells)];
        assert_eq!(rank(&entries, "BASH").len(), 1);
    }

    #[test]
    fn chinese_titles_match_by_substring() {
        let entries = [entry("切换语言", "", Group::Actions)];
        assert_eq!(rank(&entries, "语言").len(), 1);
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        assert_eq!(step(0, -1, 3), 2);
        assert_eq!(step(2, 1, 3), 0);
        assert_eq!(step(1, 1, 3), 2);
        assert_eq!(step(5, 1, 3), 0, "a stale selection is clamped first");
        assert_eq!(step(0, 1, 0), 0);
    }
}
