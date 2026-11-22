//! Which operating system a server runs, read once after the shell is up.
//!
//! A host list where every server is the same letters on a tile tells nothing about
//! what is behind it; other SSH clients learn the system on the first connection and
//! draw its mark from then on. This module does the learning: on the session's own,
//! already trusted connection it runs one read-only command on a separate channel -
//! `uname -s` and `/etc/os-release`, or `ver` on a Windows shell - and reduces the
//! answer to a short identifier. Nothing is written on the server, the terminal's own
//! channel never sees the command, and a server that answers nothing useful within a
//! few seconds is simply left without a mark.

use std::time::Duration;

use russh::ChannelMsg;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

use crate::session::Command;

/// The command a POSIX shell answers: the kernel's name, then the distribution's own
/// description when it has one. Every part is read-only and silent on failure.
const POSIX_PROBE: &str = "uname -s 2>/dev/null; cat /etc/os-release 2>/dev/null";

/// What a Windows command shell answers instead; `uname` does not exist there.
const WINDOWS_PROBE: &str = "ver";

/// The most a probe reads before it stops, so a server cannot make it hold memory.
const PROBE_LIMIT: usize = 16 * 1024;

/// How long one probe may take before the server is left without a mark.
const PROBE_TIMEOUT: Duration = Duration::from_secs(6);

/// Asks the connection's own loop for the probes and returns the system it finds.
///
/// Runs on the connection's runtime beside the terminal; a failure of any kind ends
/// in `None`, because a missing mark is not something the user has to be told about.
pub(crate) async fn probe(open: UnboundedSender<Command>) -> Option<&'static str> {
    for command in [POSIX_PROBE, WINDOWS_PROBE] {
        let Some(output) = run(&open, command).await else {
            continue;
        };
        if let Some(platform) = identify(&output) {
            return Some(platform);
        }
    }
    None
}

/// Runs one command on a channel of its own and returns what it printed.
async fn run(open: &UnboundedSender<Command>, command: &str) -> Option<String> {
    let (reply, answer) = oneshot::channel();
    open.send(Command::Exec {
        command: command.to_owned(),
        reply,
    })
    .ok()?;
    let read = async {
        let mut channel = answer.await.ok()?.ok()?;
        let mut output = Vec::new();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => {
                    output.extend_from_slice(&data);
                    if output.len() > PROBE_LIMIT {
                        break;
                    }
                }
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }
        let _ = channel.close().await;
        Some(String::from_utf8_lossy(&output).into_owned())
    };
    tokio::time::timeout(PROBE_TIMEOUT, read)
        .await
        .ok()
        .flatten()
}

/// The systems a mark is drawn for, as `(identifier, name)`.
///
/// The identifier is what a host record stores, so it never changes once shipped; the
/// name is what a tooltip says.
pub const KNOWN: &[(&str, &str)] = &[
    ("debian", "Debian"),
    ("ubuntu", "Ubuntu"),
    ("centos", "CentOS"),
    ("fedora", "Fedora"),
    ("rhel", "Red Hat Enterprise Linux"),
    ("rocky", "Rocky Linux"),
    ("almalinux", "AlmaLinux"),
    ("oracle", "Oracle Linux"),
    ("alpine", "Alpine Linux"),
    ("arch", "Arch Linux"),
    ("opensuse", "openSUSE"),
    ("raspbian", "Raspberry Pi OS"),
    ("kali", "Kali Linux"),
    ("linuxmint", "Linux Mint"),
    ("gentoo", "Gentoo"),
    ("nixos", "NixOS"),
    ("openwrt", "OpenWrt"),
    ("linux", "Linux"),
    ("freebsd", "FreeBSD"),
    ("macos", "macOS"),
    ("windows", "Windows"),
];

/// The display name of a stored identifier.
#[must_use]
pub fn name(id: &str) -> Option<&'static str> {
    KNOWN
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, name)| *name)
}

/// The system a probe's output describes, as one of the [`KNOWN`] identifiers.
///
/// `/etc/os-release` is read first and by its `ID`, then by `ID_LIKE` for a
/// derivative this list does not name; the kernel's name decides the rest.
#[must_use]
pub fn identify(output: &str) -> Option<&'static str> {
    let field = |key: &str| {
        output.lines().find_map(|line| {
            let value = line.trim().strip_prefix(key)?.strip_prefix('=')?;
            Some(
                value
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_lowercase(),
            )
        })
    };
    let canonical = |id: &str| -> Option<&'static str> {
        Some(match id {
            "debian" => "debian",
            "ubuntu" => "ubuntu",
            "centos" => "centos",
            "fedora" => "fedora",
            "rhel" | "redhat" => "rhel",
            "rocky" => "rocky",
            "almalinux" => "almalinux",
            "ol" | "oracle" => "oracle",
            "alpine" => "alpine",
            "arch" | "archarm" | "manjaro" | "endeavouros" => "arch",
            id if id.starts_with("opensuse") || id == "sles" || id == "suse" => "opensuse",
            "raspbian" => "raspbian",
            "kali" => "kali",
            "linuxmint" => "linuxmint",
            "gentoo" => "gentoo",
            "nixos" => "nixos",
            "openwrt" => "openwrt",
            _ => return None,
        })
    };
    if let Some(id) = field("ID")
        && let Some(found) = canonical(&id)
    {
        return Some(found);
    }
    if let Some(like) = field("ID_LIKE")
        && let Some(found) = like.split_whitespace().find_map(canonical)
    {
        return Some(found);
    }
    let first = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    match first {
        "Linux" => Some("linux"),
        "Darwin" => Some("macos"),
        "FreeBSD" => Some("freebsd"),
        line if line.starts_with("Microsoft Windows") => Some("windows"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_names_the_distribution() {
        let debian = "Linux\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nNAME=\"Debian GNU/Linux\"\nID=debian\n";
        assert_eq!(identify(debian), Some("debian"));
        assert_eq!(
            identify("Linux\nID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n"),
            Some("rocky")
        );
        assert_eq!(identify("Linux\nID=ol\n"), Some("oracle"));
        assert_eq!(identify("Linux\nID=opensuse-leap\n"), Some("opensuse"));
    }

    #[test]
    fn an_unnamed_derivative_falls_back_to_what_it_is_like() {
        assert_eq!(
            identify("Linux\nID=pop\nID_LIKE=\"ubuntu debian\"\n"),
            Some("ubuntu")
        );
        assert_eq!(identify("Linux\nID=someos\n"), Some("linux"));
    }

    #[test]
    fn the_kernel_name_decides_without_os_release() {
        assert_eq!(identify("Darwin\n"), Some("macos"));
        assert_eq!(identify("FreeBSD\n"), Some("freebsd"));
        assert_eq!(
            identify("\r\nMicrosoft Windows [Version 10.0.20348.2655]\r\n"),
            Some("windows")
        );
        assert_eq!(identify("'uname' is not recognized\n"), None);
        assert_eq!(identify(""), None);
    }

    #[test]
    fn every_identifier_has_a_name() {
        for (id, label) in KNOWN {
            assert_eq!(name(id), Some(*label));
        }
        assert_eq!(name("plan9"), None);
    }
}
