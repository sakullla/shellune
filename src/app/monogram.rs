//! The two letters a saved record is drawn with in a list, and its tile colour.
//!
//! Server names are written to a pattern - `region-env-role`, `bitsflow-de`,
//! `bitsflow-de-xl` - so the "first letter of each part" rule gave whole groups the
//! same two letters, and a tile that says the same thing on every row says nothing.
//! A monogram is therefore chosen against every saved record, not per name:
//!
//! - Each name offers its candidates in order: the usual initials first (so a name
//!   with no rival keeps the letters it always had), then the first two letters of
//!   its first word, then its first letter with each later word's, then its first
//!   letter with any later letter.
//! - Records take their first candidate nobody took before them, in the order they
//!   were saved. An existing record keeps its letters when a new one arrives, and a
//!   search or a filter never changes them: the letters are the record's, not the
//!   list's.
//! - Only when a name has run out of letters does it share, and then the colour - a
//!   hash of the whole name, not of the letters - still tells the two apart.

use std::collections::{HashMap, HashSet};

use shellune_core::HostId;

/// The usual initials of a name: its first letter, then the number it ends in or the
/// first letter of its second word.
#[must_use]
pub fn initials(name: &str) -> String {
    let parts = words(name);
    let Some(first) = parts.first().and_then(|part| part.chars().next()) else {
        return "#".to_owned();
    };
    let mut initials: String = first.to_uppercase().collect();
    // The number a name ends in, read from its last part: a part of its own
    // (`build-01`) or the tail of a word (`web3`).
    let last = parts.last().copied().unwrap_or_default();
    let trailing_digit = last
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .next()
        .filter(|_| parts.len() > 1 || last.chars().count() > 1);
    if let Some(digit) = trailing_digit {
        initials.push(digit);
    } else if let Some(second) = parts.get(1).and_then(|part| part.chars().next()) {
        initials.extend(second.to_uppercase());
    }
    initials
}

/// The words of a name: its runs of letters and digits.
fn words(name: &str) -> Vec<&str> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Every monogram a name could be drawn with, best first and without repeats.
#[must_use]
pub fn candidates(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |candidate: String| {
        if !candidate.is_empty() && !out.contains(&candidate) {
            out.push(candidate);
        }
    };
    let usual = initials(name);
    push(usual.clone());
    let parts = words(name);
    let Some(first) = parts.first().and_then(|part| part.chars().next()) else {
        return out;
    };
    let lead: String = first.to_uppercase().collect();
    let upper = |c: char| -> String { c.to_uppercase().collect() };
    // The first word's own second letter: `bitsflow` is BI, `bero` is BE.
    if let Some(second) = parts[0].chars().nth(1) {
        push(format!("{lead}{}", upper(second)));
    }
    // The first letter with each later word's: `bitsflow-de-xl` offers BD, then BX.
    for part in parts.iter().skip(1) {
        if let Some(initial) = part.chars().next() {
            push(format!("{lead}{}", upper(initial)));
        }
    }
    // Then any later letter of the name, in order.
    for part in &parts {
        for letter in part.chars().skip(usize::from(*part == parts[0])) {
            push(format!("{lead}{}", upper(letter)));
        }
    }
    out
}

/// The monogram of every record, in the order the records were saved.
///
/// A record takes its first candidate that no earlier record took; a name that runs
/// out of candidates keeps its usual initials and shares them.
#[must_use]
pub fn assign<'a>(records: impl IntoIterator<Item = (HostId, &'a str)>) -> HashMap<HostId, String> {
    let mut taken: HashSet<String> = HashSet::new();
    let mut chosen = HashMap::new();
    for (id, name) in records {
        let options = candidates(name);
        let pick = options
            .iter()
            .find(|candidate| !taken.contains(*candidate))
            .or_else(|| options.first())
            .cloned()
            .unwrap_or_else(|| "#".to_owned());
        taken.insert(pick.clone());
        chosen.insert(id, pick);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letters(names: &[&str]) -> Vec<String> {
        let ids: Vec<HostId> = names.iter().map(|_| HostId::new()).collect();
        let chosen = assign(ids.iter().copied().zip(names.iter().copied()));
        ids.iter().map(|id| chosen[id].clone()).collect()
    }

    #[test]
    fn names_written_to_one_pattern_get_letters_of_their_own() {
        assert_eq!(
            letters(&[
                "bero-de-bf",
                "bitsflow-de",
                "bitsflow-de-xl",
                "info-de-fb",
                "nc-de-root"
            ]),
            ["BD", "BI", "BX", "ID", "ND"]
        );
    }

    #[test]
    fn a_name_without_a_rival_keeps_its_usual_initials() {
        assert_eq!(
            letters(&["stage-01", "stage-02", "web3", "Design workstation"]),
            ["S1", "S2", "W3", "DW"]
        );
    }

    #[test]
    fn a_record_keeps_its_letters_when_a_later_one_arrives() {
        let first = letters(&["bitsflow-de"]);
        let both = letters(&["bitsflow-de", "bero-de-bf"]);
        assert_eq!(first[0], both[0], "the earlier record is not renamed");
        assert_ne!(both[0], both[1]);
    }

    #[test]
    fn a_name_that_runs_out_of_letters_shares_its_usual_initials() {
        // "ab" is A, a second "ab" takes AB, and a third has nothing left but A.
        assert_eq!(letters(&["ab", "ab", "ab"]), ["A", "AB", "A"]);
    }
}
