//! Colours for the parts of plain output a reader looks for.
//!
//! A server's banner, a log tail or a `ping` says where something is and whether it
//! went wrong in plain text: an address, a link, an "error". Other SSH clients colour
//! those; a terminal that draws them in the same ink as everything else makes the
//! reader find them by reading every line.
//!
//! Only text the program left in the default foreground is touched: a program that
//! coloured its own output keeps its colours, and the alternate screen (an editor, a
//! pager, `htop`) is never recoloured - its layout is the program's. The colours are
//! the palette's own slots, so the light theme maps them like any other ANSI colour.

use std::ops::Range;
use std::sync::LazyLock;

use alacritty_terminal::vte::ansi::Rgb;
use regex::Regex;

use crate::palette::ANSI;

/// What a highlighted span is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An IPv4 or IPv6 address, with a port or a prefix length when one follows.
    Address,
    /// An `http`, `https`, `ssh` or `ftp` link.
    Link,
    /// A word that says something failed.
    Failure,
    /// A word that says something needs a look.
    Warning,
}

impl Kind {
    /// The palette slot the span is drawn in.
    #[must_use]
    pub fn colour(self) -> Rgb {
        match self {
            Self::Address => ANSI[6],
            Self::Link => ANSI[4],
            Self::Failure => ANSI[1],
            Self::Warning => ANSI[3],
        }
    }
}

static PATTERNS: LazyLock<[(Kind, Regex); 4]> = LazyLock::new(|| {
    let compile = |pattern: &str| Regex::new(pattern).expect("a valid highlight pattern");
    [
        (
            Kind::Link,
            compile(r#"\b(?:https?|ssh|ftp)://[^\s<>"'`]+[^\s<>"'`.,;:!?)\]}]"#),
        ),
        (
            Kind::Address,
            compile(concat!(
                // IPv4, each octet 0-255, with an optional :port or /prefix.
                r"\b(?:(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)\.){3}(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)",
                r"(?::\d{1,5}|/\d{1,2})?\b",
                // IPv6 written in full or with "::" - never a clock: a time has at
                // most two colons and no hex letters.
                r"|(?:\b[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4}){7}\b)",
                r"|(?:\b(?:[0-9A-Fa-f]{1,4}:){1,7}:(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4}){0,6})?(?:/\d{1,3})?)",
                r"|(?:::(?:[0-9A-Fa-f]{1,4}(?::[0-9A-Fa-f]{1,4}){0,6})(?:/\d{1,3})?\b)",
            )),
        ),
        (
            Kind::Failure,
            compile(r"(?i)\b(?:error|errors|failed|failure|fatal|denied|refused|panic)\b"),
        ),
        (
            Kind::Warning,
            compile(r"(?i)\b(?:warning|warn|deprecated)\b"),
        ),
    ]
});

/// Whether `url` is an external web link safe to hand to the operating system (`http://` or `https://`).
///
/// Other schemes (`ssh://`, `ftp://`, `file://`, custom handlers) are deliberately refused so
/// terminal output can never trigger arbitrary local commands or protocol handlers.
#[must_use]
/// Why an OSC 8 target is not opened (R1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkRefusal {
    /// The target names a scheme outside http, https, mailto and file.
    Scheme(String),
    /// The target has no scheme, is empty, or holds whitespace or control characters.
    Malformed,
}

/// Decides whether an OSC 8 target may be opened (R1).
///
/// A program chooses these targets freely, and a link's text need not resemble it,
/// so only schemes whose handlers merely show something are allowed: web pages,
/// a mail draft, a local file. Everything else - `javascript:`, a custom protocol
/// handler, a target without a scheme - is refused and named, never opened.
///
/// # Errors
///
/// Returns why the target is refused.
pub fn osc8_target_allowed(uri: &str) -> Result<(), LinkRefusal> {
    if uri.is_empty() || uri.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err(LinkRefusal::Malformed);
    }
    let Some((scheme, rest)) = uri.split_once(':') else {
        return Err(LinkRefusal::Malformed);
    };
    let valid_scheme = scheme
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'));
    if !valid_scheme {
        return Err(LinkRefusal::Malformed);
    }
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" if is_openable_http_url(uri) => Ok(()),
        "http" | "https" => Err(LinkRefusal::Malformed),
        "mailto" if !rest.is_empty() => Ok(()),
        "file" if rest.starts_with("//") => Ok(()),
        "mailto" | "file" => Err(LinkRefusal::Malformed),
        other => Err(LinkRefusal::Scheme(other.to_owned())),
    }
}

pub fn is_openable_http_url(url: &str) -> bool {
    let rest = if url
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        &url[7..]
    } else if url
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        &url[8..]
    } else {
        return false;
    };
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..host_end];
    !authority.is_empty()
        && authority.chars().any(|ch| ch.is_ascii_alphanumeric())
        && !url.chars().any(|ch| ch.is_whitespace() || ch.is_control())
}

/// The highlighted spans of one line, as byte ranges, in order and without overlap.
///
/// A link wins over the address inside it, and an earlier span over a later one that
/// starts inside it.
#[must_use]
pub fn spans(line: &str) -> Vec<(Range<usize>, Kind)> {
    let mut found: Vec<(Range<usize>, Kind)> = Vec::new();
    for (kind, pattern) in PATTERNS.iter() {
        for hit in pattern.find_iter(line) {
            let range = hit.range();
            if found
                .iter()
                .any(|(taken, _)| taken.start < range.end && range.start < taken.end)
            {
                continue;
            }
            found.push((range, *kind));
        }
    }
    found.sort_by_key(|(range, _)| range.start);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<(&str, Kind)> {
        spans(line)
            .into_iter()
            .map(|(range, kind)| (&line[range], kind))
            .collect()
    }

    #[test]
    fn an_address_in_a_login_banner_is_found() {
        assert_eq!(
            kinds("Last login: Sun Nov 22 12:32:11 2026 from 103.100.176.124"),
            [("103.100.176.124", Kind::Address)],
            "the clock is not an address"
        );
    }

    #[test]
    fn ports_prefixes_and_ipv6_belong_to_the_address() {
        assert_eq!(
            kinds("listen 0.0.0.0:22 and 10.0.0.0/8"),
            [("0.0.0.0:22", Kind::Address), ("10.0.0.0/8", Kind::Address)]
        );
        assert_eq!(
            kinds("inet6 fe80::1c2b:3aff:fe4d:5e6f/64 scope link"),
            [("fe80::1c2b:3aff:fe4d:5e6f/64", Kind::Address)]
        );
        assert_eq!(kinds("ping ::1"), [("::1", Kind::Address)]);
    }

    #[test]
    fn a_version_or_an_out_of_range_octet_is_not_an_address() {
        assert!(kinds("kernel 6.12.74 build 1.2.3").is_empty());
        assert!(kinds("999.1.1.1").is_empty());
    }

    #[test]
    fn a_link_keeps_its_address_and_drops_trailing_punctuation() {
        assert_eq!(
            kinds("see https://10.0.0.5:8443/status."),
            [("https://10.0.0.5:8443/status", Kind::Link)]
        );
    }

    #[test]
    fn failures_and_warnings_are_whole_words() {
        assert_eq!(
            kinds("Error: connection refused; warning: retrying"),
            [
                ("Error", Kind::Failure),
                ("refused", Kind::Failure),
                ("warning", Kind::Warning)
            ]
        );
        assert!(kinds("errorless terror warnings_off").is_empty());
    }

    #[test]
    fn only_http_and_https_links_pass_the_openable_allowlist() {
        assert!(is_openable_http_url("https://example.com/docs?q=1"));
        assert!(is_openable_http_url("http://10.0.0.5:8080/health"));
        assert!(is_openable_http_url("HTTPS://EXAMPLE.COM"));
        assert!(!is_openable_http_url("ssh://root@10.0.0.5"));
        assert!(!is_openable_http_url("ftp://files.example.com/pub"));
        assert!(!is_openable_http_url("file:///etc/passwd"));
        assert!(!is_openable_http_url("javascript:alert(1)"));
        assert!(!is_openable_http_url("https://"));
    }

    #[test]
    fn osc8_targets_open_only_for_web_mail_and_file_schemes() {
        assert_eq!(osc8_target_allowed("https://example.com"), Ok(()));
        assert_eq!(osc8_target_allowed("HTTP://example.com/a"), Ok(()));
        assert_eq!(osc8_target_allowed("mailto:ops@example.com"), Ok(()));
        assert_eq!(osc8_target_allowed("file:///var/log/syslog"), Ok(()));
        assert_eq!(osc8_target_allowed("file://host/etc/hosts"), Ok(()));
        assert_eq!(
            osc8_target_allowed("javascript:alert(1)"),
            Err(LinkRefusal::Scheme("javascript".into()))
        );
        assert_eq!(
            osc8_target_allowed("vscode://file/x"),
            Err(LinkRefusal::Scheme("vscode".into()))
        );
        assert_eq!(
            osc8_target_allowed("/etc/passwd"),
            Err(LinkRefusal::Malformed)
        );
        assert_eq!(osc8_target_allowed(""), Err(LinkRefusal::Malformed));
        assert_eq!(osc8_target_allowed("https://"), Err(LinkRefusal::Malformed));
        assert_eq!(osc8_target_allowed("mailto:"), Err(LinkRefusal::Malformed));
        assert_eq!(
            osc8_target_allowed("file:relative"),
            Err(LinkRefusal::Malformed)
        );
        assert_eq!(
            osc8_target_allowed("https://a.example/ b"),
            Err(LinkRefusal::Malformed)
        );
        assert_eq!(
            osc8_target_allowed("1http://x"),
            Err(LinkRefusal::Malformed)
        );
        assert!(!is_openable_http_url("https:///only-path"));
        assert!(!is_openable_http_url("https://example.com/bad\x07url"));
    }
}
