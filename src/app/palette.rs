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

use shellune_core::{HostId, SessionId, SnippetId};

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
    /// Marks a saved record of any protocol as a favourite, or clears the mark (R1).
    ToggleFavorite(HostId),
    /// Goes to one page.
    GoTo(Destination),
    /// Opens the form for a new saved host.
    NewHost,
    /// Opens the SSH configuration import.
    ImportSshConfig,
    /// Opens the vault workbench on the home page.
    OpenVault,
    /// Opens the new shared password dialog in the vault.
    NewVaultCredential,
    /// Opens the SSH key generation dialog in the vault.
    GenerateSshKey,
    /// Opens the SSH private key import dialog in the vault.
    ImportSshKey,
    /// Opens the host trust settings page.
    OpenHostTrust,
    /// Locks the device vault immediately.
    LockVault,
    /// Unlocks the device vault.
    UnlockVault,
    /// Connects with an address that is not saved as a host first.
    QuickConnect,
    /// Connects directly to the quick-connect target typed into the palette query.
    QuickConnectDirect,
    /// Types a saved command into the terminal on screen, without running it (R6).
    InsertSnippet(SnippetId),
    /// Types a saved command into the terminal on screen and submits it (R6).
    RunSnippet(SnippetId),
    /// Opens the manager of the saved commands.
    ManageSnippets,
    /// Opens the manager on a command that has not been written yet.
    NewSnippet,
    /// Opens the global file transfers dialog.
    OpenTransfers,
    /// Opens the batch tasks centre.
    OpenBatchTasks,
    /// Closes the session on screen.
    CloseTab(SessionId),
    /// Starts the ended session of the pane on screen again, in its own tab (R8).
    Reconnect(SessionId),
    /// Gives the tab of an open session a name of its own.
    RenameTab(SessionId),
    /// Drops the name of an open session's tab and shows its automatic title again.
    RestoreTabTitle(SessionId),
    /// Steps the distinguishing colour of an open session's tab.
    CycleTabAccent(SessionId),
    /// Clears the distinguishing colour of an open session's tab.
    ClearTabAccent(SessionId),
    /// Splits the pane on screen to the right, the way the shortcut does.
    SplitRight,
    /// Splits the pane on screen downwards.
    SplitDown,
    /// Lets the active pane fill its tab, or shows every pane again.
    TogglePaneZoom,
    /// Hides the window chrome around the session on screen, or brings it back.
    ToggleFocusMode,
    /// Collapses the sidebar to its icons, or expands it.
    ToggleSidebar,
    /// Opens the search bar of the terminal on screen.
    SearchTerminal(SessionId),
    /// Starts logging the output of the terminal on screen (R3).
    StartSessionLog(SessionId, shellune_terminal::LogFormat),
    /// Stops logging the output of the terminal on screen (R3).
    StopSessionLog(SessionId),
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
    /// Saved shortcut commands (R6).
    Snippets,
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
        if all.len() <= MAX_RESULTS {
            return all;
        }
        return balanced_unfiltered(&all);
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

/// Balances an unfiltered palette when saved records exceed [`MAX_RESULTS`], keeping
/// navigation and window actions reachable instead of truncating everything after Hosts.
fn balanced_unfiltered(sorted: &[Entry]) -> Vec<Entry> {
    const RECENT_CAP: usize = 8;
    const SNIPPETS_CAP: usize = 10;
    let fixed_count = sorted
        .iter()
        .filter(|entry| {
            matches!(
                entry.group,
                Group::Sessions | Group::Shells | Group::Navigate | Group::Actions
            )
        })
        .count();
    let recent_total = sorted
        .iter()
        .filter(|entry| entry.group == Group::Recent)
        .count();
    let snippets_total = sorted
        .iter()
        .filter(|entry| entry.group == Group::Snippets)
        .count();
    let recent_cap = recent_total.min(RECENT_CAP);
    let snippets_cap = snippets_total.min(SNIPPETS_CAP);
    let hosts_cap = MAX_RESULTS
        .saturating_sub(fixed_count + recent_cap + snippets_cap)
        .max(4);

    let mut out = Vec::with_capacity(MAX_RESULTS);
    let mut recent_kept = 0;
    let mut hosts_kept = 0;
    let mut snippets_kept = 0;
    for entry in sorted {
        let keep = match entry.group {
            Group::Recent => {
                let ok = recent_kept < recent_cap;
                recent_kept += usize::from(ok);
                ok
            }
            Group::Hosts => {
                let ok = hosts_kept < hosts_cap;
                hosts_kept += usize::from(ok);
                ok
            }
            Group::Snippets => {
                let ok = snippets_kept < snippets_cap;
                snippets_kept += usize::from(ok);
                ok
            }
            Group::Sessions | Group::Shells | Group::Navigate | Group::Actions => true,
        };
        if keep {
            out.push(entry.clone());
            if out.len() == MAX_RESULTS {
                break;
            }
        }
    }
    out
}

/// How well an entry matches every word, or `None` when one word is missing.
fn score(entry: &Entry, words: &[String]) -> Option<u32> {
    let title = entry.title.to_lowercase();
    let detail = entry.detail.to_lowercase();
    let mut total = 0;
    for word in words {
        let word_len = word.chars().count();
        total += if title.starts_with(word.as_str()) {
            10
        } else if title
            .split(|c: char| !c.is_alphanumeric())
            .any(|part| part.starts_with(word.as_str()))
        {
            8
        } else if title.contains(word.as_str()) {
            6
        } else if detail.contains(word.as_str()) {
            4
        } else if word_len >= 3 && is_subsequence(word, &title) {
            2
        } else if word_len >= 3 && is_subsequence(word, &detail) {
            1
        } else {
            return None;
        };
    }
    Some(total)
}

/// Where the words of `query` were found in `text`, as merged byte ranges, so the
/// palette can draw why an entry matched.
///
/// Each word is looked for case-insensitively: first at the start of a part of the
/// text ("web" in "prod-web-01"), then anywhere in it, and finally (for words of at
/// least three characters) as an in-order subsequence ("bsn" in "bastion"). The ranges
/// always fall on character boundaries, whatever script the text is in.
#[must_use]
pub fn highlights(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let same = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    let found_at = |start: usize, word: &[char]| {
        start + word.len() <= chars.len()
            && word
                .iter()
                .zip(&chars[start..])
                .all(|(wanted, (_, letter))| same(*wanted, *letter))
    };
    let byte_end = |end: usize| chars.get(end).map_or(text.len(), |(byte, _)| *byte);
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for word in query.split_whitespace() {
        let word: Vec<char> = word.chars().collect();
        let at_part = (0..chars.len()).find(|&start| {
            (start == 0 || !chars[start - 1].1.is_alphanumeric()) && found_at(start, &word)
        });
        if let Some(start) =
            at_part.or_else(|| (0..chars.len()).find(|&start| found_at(start, &word)))
        {
            ranges.push(chars[start].0..byte_end(start + word.len()));
        } else if word.len() >= 3 {
            let mut matched = Vec::with_capacity(word.len());
            let mut cursor = 0;
            let mut all_found = true;
            for &wanted in &word {
                if let Some(offset) = chars[cursor..]
                    .iter()
                    .position(|(_, letter)| same(wanted, *letter))
                {
                    let idx = cursor + offset;
                    matched.push(chars[idx].0..byte_end(idx + 1));
                    cursor = idx + 1;
                } else {
                    all_found = false;
                    break;
                }
            }
            if all_found {
                ranges.extend(matched);
            }
        }
    }
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
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
    fn a_match_is_marked_where_each_word_was_found() {
        assert_eq!(highlights("prod-web-01", "web"), vec![5..8]);
        assert_eq!(highlights("Prod-Web-01", "WEB"), vec![5..8], "any case");
        // A part's start wins over an earlier match inside a word.
        assert_eq!(highlights("webweb web", "web"), vec![0..3]);
        assert_eq!(highlights("database db", "db"), vec![9..11]);
        // Several words, merged where they touch, in text order.
        assert_eq!(
            highlights("deploy@10.20.1.11", "10 deploy"),
            vec![0..6, 7..9]
        );
        assert_eq!(highlights("staging", "sta ging"), vec![0..7]);
        // A word of at least three letters matched as a subsequence marks its characters;
        // a word not present at all marks nothing.
        assert_eq!(highlights("bastion", "bsn"), vec![0..1, 2..3, 6..7]);
        assert!(highlights("bastion", "xyz").is_empty());
        assert!(highlights("bastion", "").is_empty());
        // Ranges fall on character boundaries in any script.
        let text = "新建本地终端";
        let ranges = highlights(text, "本地");
        assert_eq!(ranges.len(), 1);
        assert_eq!(&text[ranges[0].clone()], "本地");
    }

    #[test]
    fn detail_subsequence_matches_and_ranks_below_title_subsequence() {
        let entries = [
            entry("gateway", "deploy@production-api.internal", Group::Hosts),
            entry("production-web", "root@10.0.0.1", Group::Hosts),
        ];
        let ranked = rank(&entries, "pdw");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].title, "production-web");

        let detail_ranked = rank(&entries, "dpi");
        assert_eq!(detail_ranked.len(), 1);
        assert_eq!(detail_ranked[0].title, "gateway");
        assert!(
            !highlights(&detail_ranked[0].detail, "dpi").is_empty(),
            "detail subsequence match highlights its characters"
        );
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
