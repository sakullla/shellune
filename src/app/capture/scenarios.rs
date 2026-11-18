//! The capture scenarios: what the tool can photograph, and how each one is built.
//!
//! A scenario is one state of the real [`ShelluneView`]: a page, a terminal, a tool
//! open over a page. Nothing here is a copy of the interface. Each scenario says
//!
//! * where the shell starts ([`Scenario::start`]), with the destination the shot is
//!   about already selected;
//! * what data it needs on disk before the view exists ([`Scenario::prepare`], written
//!   into the run's own temporary root);
//! * what state it puts the view in afterwards ([`Scenario::install`]);
//! * what "arrived" means ([`Scenario::ready`]), read from the same model the render
//!   reads.
//!
//! The evidence boundary is part of the scenario, not of the report text: a shot of a
//! terminal fed with bytes proves layout, colour and text rendering, and proves nothing
//! about a live shell. Later tasks (the merged header, the host list, the batch panel)
//! add their own entries here rather than editing a shot list.
//!
//! No scenario dials a host, starts a shell, opens a PTY or runs a command: sessions are
//! display-only, and every secret store is in memory (R1, R2).

use std::path::Path;

use gpui::{App, Context, Window};

use shellune_core::{
    HostConfig, LocalShell, SessionFailure, SessionId, SessionState, SnippetBook, SplitAxis,
};
use shellune_storage::{
    HostEdit, HostStore, KeymapEntry, KeymapPreferences, Preferences, PreferencesStore,
    SnippetStore,
};

use crate::app::ShelluneView;
use crate::app::copy::Locale;
use crate::app::desktop::scripted::{ScriptedLink, solid};
use crate::app::desktop::{DriveProblem, LinkError, LinkEvent, LinkFailure, LinkState, Notice};
use crate::app::files::FileSource;
use crate::app::hosts::{ConnectionChoice, ConnectionFormField};
use crate::app::keymap::Problem;
use crate::app::nav::{Destination, Platform};
use crate::app::serial::PortScan;
use crate::app::terminal::PaneState;
use crate::app::theme::{self, Motion, Zoom};
use crate::app::view::settings::Item;
use crate::app::workspace::{AttentionKind, SectionState, Workspace};

/// What kind of surface a scenario shows, so the gallery can group them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    /// The hosts home in one of its states.
    Home,
    /// A terminal with a session around it.
    Terminal,
    /// A remote desktop session: an RDP or VNC surface in its tab.
    Desktop,
    /// The settings page.
    Settings,
    /// A tool or palette open over a page.
    Overlay,
    /// The window's start, before its first page (R7).
    Startup,
    /// The component catalogue: every shared component in every state (ADR-2).
    Design,
}

impl Category {
    /// Stable identifier written to the manifest.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Terminal => "terminal",
            Self::Desktop => "desktop",
            Self::Settings => "settings",
            Self::Overlay => "overlay",
            Self::Startup => "startup",
            Self::Design => "design",
        }
    }
}

/// One shot of a scenario: a palette, a window size, a text size and a language.
#[derive(Clone, Copy, Debug)]
pub struct Variant {
    /// Palette to draw in. Never [`theme::ThemeChoice::System`]: a shot must not depend
    /// on the machine it runs on.
    pub theme: theme::ThemeChoice,
    /// Logical client width in pixels.
    pub width: f32,
    /// Logical client height in pixels.
    pub height: f32,
    /// Text size the shot is drawn at.
    pub zoom: Zoom,
    /// Language of the strings the shot renders.
    pub locale: Locale,
}

impl Variant {
    /// The dark defaults every scenario is shot in.
    pub const fn dark() -> Self {
        Self {
            theme: theme::ThemeChoice::Dark,
            width: crate::app::DEFAULT_WINDOW_SIZE.0,
            height: crate::app::DEFAULT_WINDOW_SIZE.1,
            zoom: Zoom::Regular,
            locale: Locale::English,
        }
    }

    /// The same shot in the light palette.
    pub const fn light() -> Self {
        Self {
            theme: theme::ThemeChoice::Light,
            ..Self::dark()
        }
    }

    /// The same shot at the smallest supported window (720x480).
    pub const fn smallest(self) -> Self {
        Self {
            width: crate::app::MIN_WINDOW_SIZE.0,
            height: crate::app::MIN_WINDOW_SIZE.1,
            ..self
        }
    }

    /// The same shot at 115% text.
    pub const fn comfortable(self) -> Self {
        Self {
            zoom: Zoom::Comfortable,
            ..self
        }
    }

    /// The same shot at 200% text, the ceiling of the text-size setting (R5).
    pub const fn largest(self) -> Self {
        Self {
            zoom: Zoom::Largest,
            ..self
        }
    }

    /// The same shot at a named logical size, for layouts a default window cannot show.
    pub const fn sized(self, width: f32, height: f32) -> Self {
        Self {
            width,
            height,
            ..self
        }
    }

    /// The same shot in Chinese.
    pub const fn chinese(self) -> Self {
        Self {
            locale: Locale::Chinese,
            ..self
        }
    }

    /// `dark` or `light`, as the manifest and the file name spell it.
    #[must_use]
    pub fn theme_name(self) -> &'static str {
        match self.theme {
            theme::ThemeChoice::Dark => "dark",
            theme::ThemeChoice::Light => "light",
            theme::ThemeChoice::System => "system",
        }
    }

    /// `en` or `zh`, as the manifest spells it.
    #[must_use]
    pub fn language_name(self) -> &'static str {
        match self.locale {
            Locale::English => "en",
            Locale::Chinese => "zh",
        }
    }

    /// Text size in percent, as the manifest and the file name spell it.
    #[must_use]
    pub fn zoom_percent(self) -> u32 {
        (self.zoom.factor() * 100.0).round() as u32
    }

    /// Base file name of the PNG: scenario, size, theme, and the two modifiers only
    /// when they are not the default.
    #[must_use]
    pub fn file_stem(self, scenario: &str) -> String {
        let mut stem = format!(
            "{scenario}-{}x{}-{}",
            self.width as u32,
            self.height as u32,
            self.theme_name()
        );
        if self.zoom != Zoom::Regular {
            stem.push_str(&format!("-z{}", self.zoom_percent()));
        }
        if self.locale != Locale::English {
            stem.push('-');
            stem.push_str(self.language_name());
        }
        stem
    }
}

/// One state the tool can capture.
pub struct Scenario {
    /// Stable identifier, also the pattern `--only` matches and the file name prefix.
    pub id: &'static str,
    /// What the shot shows, for a person reading the gallery.
    pub title: &'static str,
    /// Which kind of surface it is.
    pub category: Category,
    /// What data the shot was built from, said in one line.
    pub fixture: &'static str,
    /// What this shot cannot prove.
    pub evidence: &'static str,
    /// Whether the quick run covers it.
    pub quick: bool,
    /// Shots beyond the dark and light pair every scenario gets.
    pub variants: &'static [Variant],
    /// Where the shell starts: the destination the shot is about, already selected.
    pub start: fn() -> Workspace,
    /// Data written into the run's temporary root before the view exists.
    pub prepare: fn(&Path) -> Result<(), String>,
    /// State the view is put into after it exists.
    pub install:
        fn(&mut ShelluneView, &mut Window, &mut Context<ShelluneView>) -> Result<(), String>,
    /// Whether the shot has arrived, read from the model the render reads.
    pub ready: fn(&ShelluneView, &App) -> Result<(), String>,
}

impl Scenario {
    /// Every shot this scenario is taken in: the dark and light pair at the design
    /// window, then whatever else it declares.
    #[must_use]
    pub fn shots(&self) -> Vec<Variant> {
        let mut shots = vec![Variant::dark(), Variant::light()];
        shots.extend_from_slice(self.variants);
        shots
    }
}

/// The dark and light default for a workspace that starts on `destination`.
fn page(destination: Destination) -> Workspace {
    let mut workspace = Workspace::new();
    workspace.select(destination);
    workspace
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The shell a terminal shot is titled after. Nothing is started from this name: the
/// pane it appears on has no process.
const SHOT_SHELL: &str = "pwsh";

/// What a serial device would have sent, for the shot where the port is open.
///
/// The bytes are a boot banner of the kind a console cable shows. They were fed to a
/// display-only pane: no device sent them, and the shot does not claim one did.
const SERIAL_TRANSCRIPT: &str = "\x1b[1mU-Boot 2024.01 (Jan 12 2026 - 09:14:02)\x1b[0m\r\n\
                                DRAM:  512 MiB\r\n\
                                Loading environment from SPI Flash... OK\r\n\
                                In:    serial\r\n\
                                Out:   serial\r\n\
                                Err:   serial\r\n\
                                Net:   eth0: ethernet@1c30000\r\n\
                                Hit any key to stop autoboot:  0\r\n\
                                => ";

/// The identifiers of the fixture hosts.
///
/// A saved host is identified by a random UUID, and the list tints a row from that
/// identifier. Seeding fresh identifiers would make the same scenario differ from one run
/// to the next, which is exactly what a baseline must not do, so the fixture writes the
/// identifiers a previous run would have left.
const HOST_IDS: [&str; 6] = [
    "00000000-0000-4000-8000-0000000000b1",
    "00000000-0000-4000-8000-0000000000b2",
    "00000000-0000-4000-8000-0000000000b3",
    "00000000-0000-4000-8000-0000000000b4",
    "00000000-0000-4000-8000-0000000000b5",
    "00000000-0000-4000-8000-0000000000b6",
];

/// The hosts themselves: name, address, user, and the group the list shows them under.
///
/// Addresses are from the documentation ranges (RFC 5737), so a capture never points at a
/// machine that could answer.
const HOST_ROWS: [(&str, &str, &str, Option<&str>); 6] = [
    ("build-01", "192.0.2.11", "deploy", Some("Prod")),
    ("api-02", "192.0.2.24", "deploy", Some("Prod")),
    ("edge-03", "198.51.100.7", "ops", Some("Prod")),
    ("stage-01", "198.51.100.31", "deploy", Some("Staging")),
    ("stage-02", "198.51.100.32", "deploy", Some("Staging")),
    ("lab-07", "203.0.113.19", "lab", None),
];

/// Builds the fixture hosts as records, with the identifiers above.
fn saved_hosts() -> Result<Vec<HostConfig>, String> {
    let mut hosts = Vec::new();
    for (id, (name, address, user, group)) in HOST_IDS.iter().zip(HOST_ROWS) {
        let record = serde_json::json!({
            "id": id,
            "name": name,
            "address": address,
            "port": 22,
            "user": user,
            "group": group,
        });
        let host: HostConfig = serde_json::from_value(record)
            .map_err(|error| format!("the host fixture {name} is not a valid record: {error}"))?;
        hosts.push(host);
    }
    Ok(hosts)
}

/// The eighteen more hosts the home page's shots list beside the six above: 24 in all,
/// in nested groups and without one, so the list is longer than a window and the sidebar
/// has a tree to show (ADR-3).
const HOME_ROWS: [(&str, &str, &str, Option<&str>); 18] = [
    ("web-01", "192.0.2.41", "deploy", Some("Prod/Web")),
    ("web-02", "192.0.2.42", "deploy", Some("Prod/Web")),
    ("web-03", "192.0.2.43", "deploy", Some("Prod/Web")),
    ("db-primary", "192.0.2.51", "postgres", Some("Prod/DB")),
    ("db-replica", "192.0.2.52", "postgres", Some("Prod/DB")),
    ("cache-01", "192.0.2.61", "ops", Some("Prod")),
    ("queue-01", "192.0.2.62", "ops", Some("Prod")),
    ("stage-db", "198.51.100.41", "postgres", Some("Staging")),
    ("stage-web", "198.51.100.42", "deploy", Some("Staging")),
    ("ci-runner-01", "198.51.100.61", "ci", Some("CI")),
    ("ci-runner-02", "198.51.100.62", "ci", Some("CI")),
    ("ci-cache", "198.51.100.63", "ci", Some("CI")),
    ("bastion", "203.0.113.5", "jump", None),
    ("monitoring", "203.0.113.21", "ops", None),
    ("backup-nas", "203.0.113.30", "backup", None),
    ("lab-08", "203.0.113.20", "lab", None),
    ("printer-gw", "203.0.113.40", "admin", None),
    ("design-ws", "203.0.113.50", "design", None),
];

/// How many hosts the home page's shots hold.
const HOME_HOSTS: usize = 24;

/// Builds the 24 hosts of the home page's shots: the six above, then [`HOME_ROWS`].
fn home_hosts() -> Result<Vec<HostConfig>, String> {
    let mut hosts = saved_hosts()?;
    for (number, (name, address, user, group)) in HOME_ROWS.iter().enumerate() {
        let record = serde_json::json!({
            "id": format!("00000000-0000-4000-8000-000000000c{number:02}"),
            "name": name,
            "address": address,
            "port": if number % 5 == 3 { 2222 } else { 22 },
            "user": user,
            "group": group,
        });
        let host: HostConfig = serde_json::from_value(record)
            .map_err(|error| format!("the host fixture {name} is not a valid record: {error}"))?;
        hosts.push(host);
    }
    Ok(hosts)
}

/// Writes the 24 hosts with five of them reached before, stamped relative to the moment
/// the fixture is written, and three of them marked as favourites (ADR-3). Nothing was
/// dialled: the stamps are written, not earned.
fn seed_home(root: &Path) -> Result<(), String> {
    let mut book = shellune_storage::HostBook::new();
    let hosts = home_hosts()?;
    for host in &hosts {
        book.apply(&HostEdit::create(host.clone()))
            .map_err(|error| format!("the host fixture could not be built: {error}"))?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("the clock is before 1970: {error}"))?
        .as_secs();
    // api-02 two hours ago, build-01 yesterday, stage-01 five days ago, web-01 ten
    // minutes ago and bastion three weeks ago.
    for (index, age) in [
        (1usize, 2 * 3_600u64),
        (0, 30 * 3_600),
        (3, 5 * 86_400),
        (6, 600),
        (18, 21 * 86_400),
    ] {
        book.record_connection(hosts[index].id(), now.saturating_sub(age))
            .map_err(|error| format!("the history fixture could not be built: {error}"))?;
    }
    // edge-03, web-01 and lab-07 are the favourites, in that order.
    for index in [2usize, 6, 5] {
        book.set_favorite(hosts[index].id(), true)
            .map_err(|error| format!("the favourite fixture could not be built: {error}"))?;
    }
    HostStore::at(root)
        .save(&book)
        .map_err(|error| format!("the host fixture could not be written: {error}"))?;
    Ok(())
}

/// Writes the saved hosts as a previous run would have left them.
fn seed_hosts(root: &Path) -> Result<(), String> {
    let mut book = shellune_storage::HostBook::new();
    for host in saved_hosts()? {
        book.apply(&HostEdit::create(host))
            .map_err(|error| format!("the host fixture could not be built: {error}"))?;
    }
    HostStore::at(root)
        .save(&book)
        .map_err(|error| format!("the host fixture could not be written: {error}"))?;
    Ok(())
}

/// Writes three saved commands, so the manager has rows to show: two one-liners and one
/// block, whose line count the row has to say out loud (R17).
fn seed_snippets(root: &Path) -> Result<(), String> {
    let mut book = SnippetBook::new();
    for (name, body) in [
        ("release", "cargo test --workspace --all-features"),
        ("logs", "journalctl -u shellune -n 200 --no-pager"),
        (
            "checks",
            "cargo fmt --all -- --check\ncargo clippy --workspace --all-targets\ncargo test --workspace",
        ),
    ] {
        book.create(name, body)
            .map_err(|error| format!("the command fixture could not be built: {error}"))?;
    }
    SnippetStore::at(root)
        .save(&book)
        .map_err(|error| format!("the command fixture could not be written: {error}"))?;
    Ok(())
}

/// Writes a saved key map in which two actions answer the same combination, the way a
/// hand-edited preferences file does: the shot shows what the page says about it, and the
/// defaults that are in force instead.
fn seed_conflicting_keymap(root: &Path) -> Result<(), String> {
    let mut saved = KeymapPreferences::default();
    saved.set_for_platform(
        Platform::current().id(),
        vec![
            KeymapEntry::new("nav.hosts", "shell", Some("ctrl-alt-h".to_owned())),
            KeymapEntry::new("nav.settings", "shell", Some("ctrl-alt-h".to_owned())),
        ],
    );
    PreferencesStore::at(root)
        .save(&Preferences {
            keymap: Some(saved),
            ..Default::default()
        })
        .map_err(|error| format!("the shortcut fixture could not be written: {error}"))?;
    Ok(())
}

/// Nothing to prepare: the empty page is what a fresh data directory holds.
fn nothing(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Nothing to install: the destination is the whole state.
fn no_state(
    _: &mut ShelluneView,
    _: &mut Window,
    _: &mut Context<ShelluneView>,
) -> Result<(), String> {
    Ok(())
}

/// The bytes a terminal shot shows.
///
/// Written out rather than captured from a real shell: a shot has to be the same on every
/// machine and at every run, so the transcript is a fixture like the hosts are. It is a
/// plausible session, including colour, a Chinese line and a prompt, drawn in the same
/// grid a live shell uses.
const TRANSCRIPT: &str = "\x1b[1;36mdeploy@staging\x1b[0m:\x1b[1;34m~/project/release\x1b[0m$ git status -sb\r\n\
## main...origin/main\r\n\
 M crates/terminal/src/pty.rs\r\n\
?? docs/verification/ui/\r\n\
\x1b[1;36mdeploy@staging\x1b[0m:\x1b[1;34m~/project/release\x1b[0m$ cargo test -p shellune-terminal\r\n\
   Compiling shellune-terminal v0.4.0\r\n\
    Finished test [unoptimized + debuginfo] target(s)\r\n\
     Running unittests src/lib.rs\r\n\
running 42 tests\r\n\
test grid::wraps_at_the_last_column ... \x1b[32mok\x1b[0m\r\n\
test input::commits_a_chinese_glyph_whole ... \x1b[32mok\x1b[0m\r\n\
test pty::keeps_the_login_path ... \x1b[32mok\x1b[0m\r\n\
test paste::stages_a_confirmation ... \x1b[32mok\x1b[0m\r\n\
\r\n\
test result: \x1b[32mok\x1b[0m. 42 passed; 0 failed; 0 ignored\r\n\
\x1b[1;36mdeploy@staging\x1b[0m:\x1b[1;34m~/project/release\x1b[0m$ echo 部署完成\r\n\
部署完成\r\n\
\x1b[1;36mdeploy@staging\x1b[0m:\x1b[1;34m~/project/release\x1b[0m$ ";

/// A second shell a split shot puts beside the first. Nothing is started from it.
const SHOT_SPLIT_SHELL: &str = "zsh";

/// The bytes the second pane of a split shows: a short, different transcript, so the
/// two panes of one shot are told apart at a glance.
const SPLIT_TRANSCRIPT: &str = "\x1b[1;36mdeploy@staging\x1b[0m:\x1b[1;34m~/project/release\x1b[0m$ systemctl status shellune --no-pager\r\n\
\x1b[32m●\x1b[0m shellune.service - Shellune terminal host\r\n\
     Active: \x1b[32mactive (running)\x1b[0m since Thu 2026-11-12 09:14:03 UTC\r\n\
\x1b[1;36mdeploy@staging\x1b[0m:\x1b[1;34m~/project/release\x1b[0m$ ";

/// Puts one display-only session on screen and returns it.
fn add_session(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
    program: &str,
    content: &[u8],
) -> Result<SessionId, String> {
    let shell = LocalShell::new(program, program);
    let id = view.attach_capture_session(shell, content, Some(window), cx);
    if view.pane_of(id).is_none() {
        return Err(format!(
            "the {program} session was recorded without a terminal"
        ));
    }
    Ok(id)
}

/// Three tabs: two local shells and one saved host, with the second tab carrying a
/// background bell (R5, R7).
fn show_tabs(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let first = add_session(view, window, cx, SHOT_SHELL, TRANSCRIPT.as_bytes())?;
    let second = add_session(
        view,
        window,
        cx,
        SHOT_SPLIT_SHELL,
        SPLIT_TRANSCRIPT.as_bytes(),
    )?;
    view.capture_note_attention(second, AttentionKind::Bell);
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    view.attach_capture_remote_session(
        &host,
        b"$ uptime\r\n 09:20:11 up 12 days\r\n$ ",
        Some(window),
        cx,
    );
    // The first tab is the one the shot is about: the other two say what a strip of
    // several tabs looks like, and one of them is calling from the background.
    view.activate(first, cx);
    Ok(())
}

/// The shells of the overflowing strip: enough programs that the titles differ.
const OVERFLOW_SHELLS: [&str; 7] = ["pwsh", "zsh", "bash", "fish", "nu", "cmd", "sh"];

/// How many tabs the overflowing strip holds.
const OVERFLOW_TABS: usize = 14;

/// The tab that is current in the overflowing strip: one in the middle, so tabs run on
/// past both ends.
const OVERFLOW_CURRENT: usize = 7;

/// The name the long-titled tab carries in the tab shots.
const LONG_TAB_NAME: &str = "production-database-primary eu-west-1 log watcher";

/// More tabs than the strip has room for, with one in the middle current (R2).
fn show_tabs_overflow(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let mut ids = Vec::new();
    for index in 0..OVERFLOW_TABS {
        let program = OVERFLOW_SHELLS[index % OVERFLOW_SHELLS.len()];
        ids.push(add_session(
            view,
            window,
            cx,
            program,
            TRANSCRIPT.as_bytes(),
        )?);
    }
    view.capture_name_tab(ids[2], LONG_TAB_NAME);
    view.activate(ids[OVERFLOW_CURRENT], cx);
    Ok(())
}

/// A shell with a long name beside a saved host's tab that is connecting again (R2).
fn show_tabs_connecting(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let first = add_session(view, window, cx, SHOT_SHELL, TRANSCRIPT.as_bytes())?;
    view.capture_name_tab(first, LONG_TAB_NAME);
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    let remote = view.attach_capture_remote_session(&host, TRANSCRIPT.as_bytes(), Some(window), cx);
    view.capture_reconnecting(remote);
    // The long-named tab is the current one, so its title is drawn at full weight and
    // the connecting tab is one in the background.
    view.activate(first, cx);
    Ok(())
}

/// The connecting-tab fixture with a newer release on offer and a failed sync, the two
/// notices the top row carries on every page (R2, R9).
fn show_tabs_notices(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_tabs_connecting(view, window, cx)?;
    view.capture_update_notice(cx);
    view.capture_sync_failed(cx);
    Ok(())
}

/// One session in a tab of its own, split left and right, with the new pane active
/// (R5, R12).
fn show_split(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    add_session(view, window, cx, SHOT_SHELL, TRANSCRIPT.as_bytes())?;
    let shell = LocalShell::new(SHOT_SPLIT_SHELL, SHOT_SPLIT_SHELL);
    view.capture_split_with_session(
        SplitAxis::LeftRight,
        shell,
        SPLIT_TRANSCRIPT.as_bytes(),
        window,
        cx,
    )
    .ok_or_else(|| "the split did not produce a second pane".to_owned())?;
    Ok(())
}

/// A saved host's terminal whose connection ended (R5, R12).
fn show_disconnected(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    let id = view.attach_capture_remote_session(&host, TRANSCRIPT.as_bytes(), Some(window), cx);
    view.capture_end_session(id, SessionFailure::ConnectionLost);
    Ok(())
}

/// The serial strip, in the two states a device produces: open, and not open (R21).
///
/// Two panes: one whose port is open and carries a transcript, and one whose port did not
/// open, which names the device, the cause, the settings that were asked for and what the
/// port list really holds. No device is opened for either of them, so the shot proves what
/// the interface says in these states and nothing about a port.
fn show_serial(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let live = view.attach_capture_serial_session(
        "Console cable",
        "COM3",
        "115200 8N1, none flow control",
        PaneState::Live,
        PortScan::NotAsked,
        None,
        SERIAL_TRANSCRIPT.as_bytes(),
        Some(window),
        cx,
    );
    if view.pane_of(live).is_none() {
        return Err("the serial session was recorded without a terminal".to_owned());
    }
    let busy = "COM7: port is in use. Close the other program that holds this port, then \
                open it again.";
    view.attach_capture_serial_session(
        "Radio",
        "COM7",
        "9600 7E2, XON/XOFF flow control",
        PaneState::Failed(shellune_core::SessionFailure::ConnectionFailed {
            reason: busy.to_owned(),
        }),
        PortScan::Ready(vec![shellune_serial::PortInfo::sample(
            "COM3",
            "COM3",
            Some("USB serial adapter"),
        )]),
        Some(busy.to_owned()),
        b"",
        Some(window),
        cx,
    );
    view.activate(live, cx);
    Ok(())
}

/// Where the serial shot has arrived: two serial panes are on screen with their strips.
fn serial_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    let shown = view
        .open_sessions()
        .into_iter()
        .filter(|id| {
            view.pane_of(*id)
                .is_some_and(|pane| pane.read(cx).is_serial())
        })
        .count();
    if shown < 2 {
        return Err(format!("only {shown} serial panes are on screen"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Remote desktops
// ---------------------------------------------------------------------------

/// The desktop the scripted frames describe. Nothing is negotiated: the size is the
/// one the regions claim.
const DESKTOP_SIZE: (u32, u32) = (1280, 720);

/// A saved RDP connection for a shot. Nothing dials it.
fn capture_rdp_record() -> shellune_core::ConnectionRecord {
    shellune_core::ConnectionRecord::Rdp(
        shellune_core::RdpConfig::new("Design workstation", "10.0.0.40", Some("alice"))
            .expect("a valid RDP record"),
    )
}

/// A saved VNC connection for a shot. Nothing dials it.
fn capture_vnc_record() -> shellune_core::ConnectionRecord {
    shellune_core::ConnectionRecord::Vnc(
        shellune_core::VncConfig::new("Build monitor", "10.0.0.41").expect("a valid VNC record"),
    )
}

/// The regions the scripted desktop is made of: a wallpaper, a window with a title bar
/// and a body, and a bar along the bottom.
///
/// They are flat colours on purpose. The shot is of the chrome the application draws
/// around a picture - the controls, the zoom, the notice - and a photorealistic
/// snapshot would suggest a server drew it.
fn desktop_regions() -> Vec<LinkEvent> {
    vec![
        LinkEvent::Frame(solid(
            0,
            0,
            1280,
            720,
            DESKTOP_SIZE,
            [0x30, 0x28, 0x22, 0xff],
        )),
        LinkEvent::Frame(solid(
            120,
            96,
            640,
            420,
            DESKTOP_SIZE,
            [0xf4, 0xf4, 0xf4, 0xff],
        )),
        LinkEvent::Frame(solid(
            120,
            96,
            640,
            36,
            DESKTOP_SIZE,
            [0xb0, 0x6a, 0x2d, 0xff],
        )),
        LinkEvent::Frame(solid(
            0,
            688,
            1280,
            32,
            DESKTOP_SIZE,
            [0x2a, 0x26, 0x22, 0xff],
        )),
    ]
}

/// Mounts one scripted desktop and applies what its link already holds.
fn mount_desktop(
    view: &mut ShelluneView,
    record: &shellune_core::ConnectionRecord,
    events: Vec<LinkEvent>,
    cx: &mut Context<ShelluneView>,
) -> Result<SessionId, String> {
    let (link, _log) = ScriptedLink::new(events);
    let id = view.attach_scripted_desktop(record, Box::new(link), cx);
    let pane = view
        .desktop_of(id)
        .cloned()
        .ok_or_else(|| "the desktop surface was not mounted".to_owned())?;
    pane.update(cx, |pane, cx| {
        pane.pump_once(cx);
    });
    Ok(id)
}

/// An RDP session with its first picture: the controls, the zoom and the tab around it.
fn show_desktop_rdp(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let mut events = vec![LinkEvent::State {
        state: LinkState::Connected,
        failure: LinkFailure::None,
        detail: 0,
        reason: String::new(),
    }];
    events.extend(desktop_regions());
    let id = mount_desktop(view, &capture_rdp_record(), events, cx)?;
    // The zoom is one of the controls the shot is about, so it is off its default.
    if let Some(pane) = view.desktop_of(id).cloned() {
        pane.update(cx, |pane, cx| pane.zoom_in(cx));
    }
    Ok(())
}

/// A VNC session the server refused: the card that says which step failed and what to
/// do next.
fn show_desktop_vnc_failed(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let reason = "the server refused the password";
    let events = vec![
        LinkEvent::State {
            state: LinkState::Connecting,
            failure: LinkFailure::None,
            detail: 0,
            reason: String::new(),
        },
        LinkEvent::State {
            state: LinkState::Failed(LinkFailure::Authentication),
            failure: LinkFailure::Authentication,
            detail: 0,
            reason: reason.to_owned(),
        },
    ];
    mount_desktop(view, &capture_vnc_record(), events, cx)?;
    Ok(())
}

/// A live RDP session whose last send did not reach the remote side: the strip over the
/// picture, which is the only place that sentence can be read.
fn show_desktop_send_problem(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let mut events = vec![LinkEvent::State {
        state: LinkState::Connected,
        failure: LinkFailure::None,
        detail: 0,
        reason: String::new(),
    }];
    events.extend(desktop_regions());
    let id = mount_desktop(view, &capture_rdp_record(), events, cx)?;
    let reason = LinkError::State.reason();
    if let Some(pane) = view.desktop_of(id).cloned() {
        pane.update(cx, |pane, cx| pane.show_send_problem(&reason, cx));
    }
    Ok(())
}

/// A live RDP session that reported a folder it could not map and a clipboard the
/// server refused: the notices over the picture, each said once (R1, R5).
fn show_desktop_notices(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let mut events = vec![LinkEvent::State {
        state: LinkState::Connected,
        failure: LinkFailure::None,
        detail: 0,
        reason: String::new(),
    }];
    events.extend(desktop_regions());
    events.push(LinkEvent::Notice(Notice::DriveUnavailable(
        DriveProblem::Missing,
    )));
    events.push(LinkEvent::Notice(Notice::ClipboardRefused));
    mount_desktop(view, &capture_rdp_record(), events, cx)?;
    Ok(())
}

/// The folder the RDP editor shot shows as mapped. Nothing reads it.
const CAPTURE_DRIVE_FOLDER: &str = r"C:\Users\alice\Exchange";

/// Writes one RDP connection that maps a folder and one VNC connection with its
/// clipboard off, so each editor opens on the new fields with a value in them.
fn seed_sharing_connections(root: &Path) -> Result<(), String> {
    let mut book = shellune_storage::HostBook::new();
    let mut rdp = shellune_core::RdpConfig::new("Design workstation", "10.0.0.40", Some("alice"))
        .map_err(|error| format!("the RDP fixture is invalid: {error}"))?;
    rdp.set_drive_folder(Some(CAPTURE_DRIVE_FOLDER))
        .map_err(|error| format!("the folder fixture is invalid: {error}"))?;
    let mut vnc = shellune_core::VncConfig::new("Build monitor", "10.0.0.41")
        .map_err(|error| format!("the VNC fixture is invalid: {error}"))?;
    vnc.set_clipboard(false);
    for record in [
        shellune_core::ConnectionRecord::Rdp(rdp),
        shellune_core::ConnectionRecord::Vnc(vnc),
    ] {
        book.insert_connection(record)
            .map_err(|error| format!("the connection fixture could not be added: {error}"))?;
    }
    HostStore::at(root)
        .save(&book)
        .map_err(|error| format!("the connection fixture could not be written: {error}"))
}

/// Opens the editor of the saved connection of one protocol, the way its row's edit
/// button does.
fn show_connection_editor(
    view: &mut ShelluneView,
    protocol: shellune_core::ProtocolKind,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let id = view
        .book()
        .connections()
        .iter()
        .find(|record| record.protocol() == protocol)
        .map(shellune_core::ConnectionRecord::id)
        .ok_or_else(|| format!("no saved {} connection to edit", protocol.label()))?;
    view.edit_connection_for_test(id, cx);
    Ok(())
}

/// The RDP editor on a saved connection: the clipboard switch and the mapped folder.
fn show_rdp_editor(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_connection_editor(view, shellune_core::ProtocolKind::Rdp, cx)
}

/// The VNC editor on a saved connection: the clipboard switch, turned off.
fn show_vnc_editor(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_connection_editor(view, shellune_core::ProtocolKind::Vnc, cx)
}

/// Where an editor shot has arrived: the form is open on a saved record of that
/// protocol and holds the values the fixture wrote.
fn connection_editor_ready_for(
    view: &ShelluneView,
    protocol: shellune_core::ProtocolKind,
) -> Result<(), String> {
    let draft = view
        .hosts_state()
        .protocol_draft()
        .ok_or_else(|| "no connection form is open".to_owned())?;
    if draft.protocol() != protocol || draft.editing().is_none() {
        return Err(format!(
            "the open form is a {} form, not the saved {} connection",
            draft.protocol().label(),
            protocol.label()
        ));
    }
    Ok(())
}

fn rdp_editor_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    connection_editor_ready_for(view, shellune_core::ProtocolKind::Rdp)?;
    let folder = view
        .hosts_state()
        .protocol_draft()
        .map(|draft| draft.text(ConnectionFormField::DriveFolder).to_owned())
        .unwrap_or_default();
    if folder != CAPTURE_DRIVE_FOLDER {
        return Err(format!("the folder field holds {folder:?}"));
    }
    Ok(())
}

fn vnc_editor_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    connection_editor_ready_for(view, shellune_core::ProtocolKind::Vnc)?;
    if view
        .hosts_state()
        .protocol_draft()
        .is_some_and(|draft| draft.clipboard())
    {
        return Err("the clipboard switch is on; the fixture turned it off".to_owned());
    }
    Ok(())
}

/// Where a desktop shot has arrived: one session with a surface that has a picture.
fn desktop_live_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    let shown = view
        .open_sessions()
        .into_iter()
        .filter(|id| {
            view.desktop_of(*id)
                .is_some_and(|pane| pane.read(cx).surface().size() == DESKTOP_SIZE)
        })
        .count();
    if shown != 1 {
        return Err(format!("{shown} desktop surfaces hold a picture"));
    }
    Ok(())
}

/// Where a failed desktop shot has arrived: one session whose surface stopped with a
/// reason, whichever failure the protocol layer named.
fn desktop_failed_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    let phases: Vec<String> = view
        .open_sessions()
        .into_iter()
        .filter_map(|id| {
            view.desktop_of(id)
                .map(|pane| format!("{:?}", pane.read(cx).phase()))
        })
        .collect();
    let failed = view.open_sessions().into_iter().any(|id| {
        view.desktop_of(id)
            .is_some_and(|pane| pane.read(cx).phase().failure().is_some())
    });
    if !failed {
        return Err(format!(
            "the desktop session did not report a failure: {phases:?}"
        ));
    }
    Ok(())
}
/// Writes a hosts file that cannot be read, so the failed state is the real one.
fn damage_hosts(root: &Path) -> Result<(), String> {
    let path = HostStore::at(root.to_path_buf()).path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("the run root could not be created: {error}"))?;
    }
    std::fs::write(&path, "{ this is not a host book }")
        .map_err(|error| format!("the damaged fixture could not be written: {error}"))
}

/// Writes one book holding both kinds of record: saved hosts and desktop or serial
/// connections, so the list draws the rows of another protocol beside them.
fn seed_connections(root: &Path) -> Result<(), String> {
    let mut book = shellune_storage::HostBook::new();
    for host in saved_hosts()? {
        book.apply(&HostEdit::create(host))
            .map_err(|error| format!("the host fixture could not be built: {error}"))?;
    }
    for record in [
        shellune_core::ConnectionRecord::Rdp(
            shellune_core::RdpConfig::new("Design workstation", "10.0.0.40", Some("alice"))
                .map_err(|error| format!("the RDP fixture is invalid: {error}"))?,
        ),
        shellune_core::ConnectionRecord::Vnc(
            shellune_core::VncConfig::new("Build monitor", "10.0.0.41")
                .map_err(|error| format!("the VNC fixture is invalid: {error}"))?,
        ),
        shellune_core::ConnectionRecord::Serial(
            shellune_core::SerialConfig::new("Console cable", "COM3")
                .map_err(|error| format!("the serial fixture is invalid: {error}"))?,
        ),
    ] {
        book.insert_connection(record)
            .map_err(|error| format!("the connection fixture could not be added: {error}"))?;
    }
    HostStore::at(root)
        .save(&book)
        .map_err(|error| format!("the connection fixture could not be written: {error}"))
}

/// Where the mixed-list shot has arrived: the list holds the saved connections too.
fn connections_listed_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    let saved = view.book().connections().len();
    if saved != 3 {
        return Err(format!("{saved} connections are in the book, not three"));
    }
    Ok(())
}

/// Narrows the list with a phrase nothing matches, the way a person would.
fn show_no_match(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.search_hosts("no-such-host", cx);
    Ok(())
}

/// Types an address into the home page's search: a saved host answers it, and the
/// "connect to …" row sits on top of it (ADR-3).
fn show_direct(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.search_hosts("deploy@192.0.2.24", cx);
    Ok(())
}

/// Where the direct-connection shot has arrived: the search reads as an address.
fn direct_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    home_hosts_ready(view, cx)?;
    if crate::app::hosts::parse_quick_target(view.hosts_state().search()).is_none() {
        return Err("the search does not read as user@host".to_owned());
    }
    Ok(())
}

/// The favourites place of the sidebar (ADR-3).
fn show_home_favorites(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_home_section(crate::app::hosts::HomeSection::Favorites, cx);
    Ok(())
}

/// The sidebar folded to its icons on a wide window (ADR-3).
fn show_home_folded(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_home_fold(true, cx);
    Ok(())
}

/// Where the folded shot has arrived.
fn home_folded_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    home_hosts_ready(view, cx)?;
    if !view.home_sidebar_folded() {
        return Err("the sidebar is not folded".to_owned());
    }
    Ok(())
}

/// Where the zero-result shot has arrived: the phrase is the one the list reads, and
/// it keeps nothing.
fn no_match_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.hosts_state().search() != "no-such-host" {
        return Err("the search phrase did not reach the list".to_owned());
    }
    Ok(())
}

/// Where the failed-store shot has arrived: the hosts destination reports the failure
/// instead of an empty book.
fn hosts_failed_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    match view.workspace().state(Destination::Hosts) {
        SectionState::Failed => Ok(()),
        other => Err(format!("the hosts page is {other:?}, not failed")),
    }
}

/// Puts one display-only session on screen, titled like a real one.
fn show_terminal(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let shell = LocalShell::new(SHOT_SHELL, SHOT_SHELL);
    let id = view.attach_capture_session(shell, TRANSCRIPT.as_bytes(), Some(window), cx);
    if view.pane_of(id).is_none() {
        return Err("the session was recorded without a terminal".to_owned());
    }
    Ok(())
}

/// Shows the terminal, then opens the saved commands over it.
fn show_snippets(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_terminal(view, window, cx)?;
    view.open_snippets(false, cx);
    Ok(())
}

/// Opens the command palette over the hosts page.
fn show_palette(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.toggle_palette(window, cx);
    Ok(())
}

/// The block the paste confirmation in a capture holds: long enough that the dialog has
/// to scroll it, and made of the commands this project really runs (R17).
const PASTE_BLOCK: &str = "cargo fmt --all -- --check\n\
cargo clippy --workspace --all-targets --all-features -- -D warnings\n\
cargo test --workspace --all-features\n\
git status -sb\n\
git diff --check\n";

/// Shows the terminal, with a paste waiting for its answer as a dialog over it (R17).
fn show_paste(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_terminal(view, window, cx)?;
    let pane = view
        .pane()
        .ok_or_else(|| "the terminal destination has no pane on screen".to_owned())?
        .clone();
    pane.update(cx, |pane, cx| pane.stage_paste(PASTE_BLOCK, cx));
    Ok(())
}

/// Shows a saved host's connection dialog while the transport is connecting (R17).
fn show_connection(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    view.capture_connection(&host, cx);
    Ok(())
}

/// The local terminal with its find bar open on a query (R5).
fn show_terminal_search(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_terminal(view, window, cx)?;
    view.capture_open_search("deploy", window, cx);
    Ok(())
}

fn terminal_search_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    if !view.capture_search_open(cx) {
        return Err("the find bar is not open with a query".to_owned());
    }
    Ok(())
}

/// The connection dialog after a refused sign-in (R7).
fn show_connection_failed(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    view.capture_fail_connection(&host, cx);
    Ok(())
}

fn connection_failed_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay() != Some("connect") {
        return Err("the connection dialog is not on screen".to_owned());
    }
    if !view.capture_connection_failed() {
        return Err("the dialog is not showing a failure".to_owned());
    }
    Ok(())
}

/// The deletion question before a host is removed (R2, R7).
fn show_delete_question(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_ask_delete_first(cx);
    Ok(())
}

fn delete_question_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if !view.capture_delete_question() {
        return Err("the deletion question is not open".to_owned());
    }
    Ok(())
}

/// The empty form a new host starts from, as the dialog draws it (R5).
fn show_new_host(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_new_host(cx);
    Ok(())
}

fn new_host_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if !view.capture_new_host_open() {
        return Err("the new host form is not open".to_owned());
    }
    Ok(())
}

/// How many saved hosts the jump picker shot offers: the book size at which the old
/// wall of buttons filled half the form (R3).
const JUMP_HOSTS: usize = 24;

/// Writes the hosts the jump picker shot chooses from.
fn seed_jump_hosts(root: &Path) -> Result<(), String> {
    write_hosts(root, scale_hosts(JUMP_HOSTS)?)
}

/// The new host form with two jump hosts chosen and the picker open on the rest (R3).
fn show_new_host_jumps(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_new_host_jumps(2, window, cx);
    Ok(())
}

fn new_host_jumps_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    match view.capture_jump_picker_chain() {
        Some(2) => Ok(()),
        Some(other) => Err(format!("the chain holds {other} hops, not 2")),
        None => Err("the new host form with its jump picker is not open".to_owned()),
    }
}

/// The question asked before a session that is copying a file closes (R5, R9).
///
/// The transfer is the one the transfer strip scenario seeds: the queue holds it as a
/// worker's events would, and no byte moves. The close is asked the way the tab's close
/// button asks it, so the shot is the question and nothing was closed.
fn show_close_question(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_running_transfer(view, window, cx)?;
    let id = view
        .active_session()
        .ok_or_else(|| "no session".to_owned())?;
    view.capture_ask_close(id, cx);
    Ok(())
}

fn close_question_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if !view.capture_close_question() {
        return Err("the close question is not open".to_owned());
    }
    Ok(())
}

/// A notification over the terminal, as an action would raise it (R2).
fn show_notification(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_terminal(view, window, cx)?;
    view.capture_toast(
        theme::Tone::Positive,
        "Host saved",
        Some("web · 10.0.0.5"),
        cx,
    );
    Ok(())
}

fn notification_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_toast_count() == 0 {
        return Err("no notification is on screen".to_owned());
    }
    Ok(())
}

/// Opens the settings page on one of its items, the way the navigation does (R16).
fn show_settings_item(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
    item: Item,
) -> Result<(), String> {
    view.capture_settings_page(item, cx);
    Ok(())
}

/// Opens the shortcut page.
fn show_settings_keys(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_item(view, window, cx, Item::Shortcuts)
}

/// Opens the shortcut page with a query in its search, so the shot shows the list narrowed
/// by a combination rather than the whole catalogue (R16).
fn show_settings_keys_search(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_keys(view, window, cx)?;
    view.capture_settings_query("ctrl", cx);
    Ok(())
}

/// A saved host's session with the file view open on fixture listings (R2, R6).
///
/// Both columns answer from the fixture, so the shot shows the dual view this build
/// draws and says nothing about a server or about this computer's own disk.
fn show_files(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_files_of(view, window, cx, false)
}

/// The same view on a directory that really is empty (R6).
fn show_empty_files(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_files_of(view, window, cx, true)
}

fn show_files_of(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
    empty: bool,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    let id = view.attach_capture_remote_session(&host, TRANSCRIPT.as_bytes(), Some(window), cx);
    view.capture_files(id, empty, cx);
    if !view.files_visible(id) {
        return Err("the file view did not open".to_owned());
    }
    Ok(())
}

/// The file view with two rows of the server's column marked (R14).
fn show_marked_files(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    let id = view.attach_capture_remote_session(&host, TRANSCRIPT.as_bytes(), Some(window), cx);
    view.capture_files_marked(
        id,
        FileSource::Remote,
        &["构建 产物.tar.zst", "部署 记录.log"],
        cx,
    );
    if !view.files_visible(id) {
        return Err("the file view did not open".to_owned());
    }
    Ok(())
}

/// The file view of a session whose server refused the read (R14).
fn show_isolated_failure(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    let id = view.attach_capture_remote_session(&host, TRANSCRIPT.as_bytes(), Some(window), cx);
    view.capture_files_remote_failed(id, cx);
    if !view.files_visible(id) {
        return Err("the file view did not open".to_owned());
    }
    Ok(())
}

/// The file view with a transfer in flight (R6).
fn show_running_transfer(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_files(view, window, cx)?;
    let id = view
        .active_session()
        .ok_or_else(|| "no session".to_owned())?;
    view.capture_transfer(id, true, cx);
    Ok(())
}

/// The file view with a transfer that stopped because the connection ended (R6).
fn show_failed_transfer(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_files(view, window, cx)?;
    let id = view
        .active_session()
        .ok_or_else(|| "no session".to_owned())?;
    view.capture_transfer(id, false, cx);
    Ok(())
}

/// The forwarding page with no saved rule (R6).
fn show_forwards_empty(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    view.capture_forwards(&host, false, cx);
    Ok(())
}

/// The forwarding page with all three kinds, one of them listening (R6).
fn show_forwards_running(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    view.capture_forwards(&host, true, cx);
    Ok(())
}

/// Shows several tabs, then lists all of them over the terminal (R17).
fn show_tab_list(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_tabs(view, window, cx)?;
    view.capture_open_tab_list(window, cx);
    Ok(())
}

// ---------------------------------------------------------------------------
// Readiness
// ---------------------------------------------------------------------------

/// Whether the shell is on `destination` with its section in `state`.
fn on_page(
    view: &ShelluneView,
    destination: Destination,
    state: SectionState,
) -> Result<(), String> {
    if view.workspace().destination() != destination {
        return Err(format!(
            "the shell is on {:?}, not {destination:?}",
            view.workspace().destination()
        ));
    }
    let actual = view.workspace().state(destination);
    if actual != state {
        return Err(format!(
            "the {destination:?} section is {actual:?}, not {state:?}"
        ));
    }
    Ok(())
}

/// Whether the file view is open on the fixture directory, with the entries it holds.
fn files_ready_at(view: &ShelluneView, expected: (&str, usize)) -> Result<(), String> {
    let (open, path, entries) = view
        .capture_files_listing()
        .ok_or_else(|| "no file view is open".to_owned())?;
    if !open {
        return Err("the file view is not open".to_owned());
    }
    if path != expected.0 {
        return Err(format!(
            "the file view shows {path:?}, not {:?}",
            expected.0
        ));
    }
    if entries != expected.1 {
        return Err(format!(
            "the listing holds {entries} entries, not {}",
            expected.1
        ));
    }
    Ok(())
}

fn files_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    files_ready_at(view, ("/home/deploy/项目 发布/2026-11 版本", 4))
}

fn empty_files_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    files_ready_at(view, ("/home/deploy/备份/2026-10", 0))
}

fn marked_files_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    files_ready(view, _cx)?;
    let (marked, local) = view
        .capture_files_marks()
        .ok_or_else(|| "no file view is open".to_owned())?;
    if marked != 2 {
        return Err(format!(
            "the shot holds {marked} marked row(s), not the two the fixture marked"
        ));
    }
    if local == 0 {
        return Err("this computer's column is empty, so no batch bar could be shown".to_owned());
    }
    Ok(())
}

fn isolated_failure_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    let (failed, local) = view
        .capture_files_isolation()
        .ok_or_else(|| "no file view is open".to_owned())?;
    if !failed {
        return Err("the server's column does not state a failure".to_owned());
    }
    if local == 0 {
        return Err("the failure cleared this computer's column as well".to_owned());
    }
    Ok(())
}

fn running_transfer_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    files_ready(view, _cx)?;
    if view.capture_transfer_count() == 0 {
        return Err("the transfer strip is empty".to_owned());
    }
    match view.capture_transfer_measured() {
        Some((true, true)) => Ok(()),
        other => Err(format!(
            "the running transfer shows no measured speed and time left: {other:?}"
        )),
    }
}

fn failed_transfer_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    files_ready(view, _cx)?;
    if view.capture_transfer_count() == 0 {
        return Err("the transfer strip is empty".to_owned());
    }
    match view.capture_transfer_residue() {
        Some((true, true)) => Ok(()),
        other => Err(format!(
            "the failed transfer names no partial file or checkpoint: {other:?}"
        )),
    }
}

fn forwards_empty_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_forward_counts().0 != 0 {
        return Err("the forwarding page has saved rules".to_owned());
    }
    Ok(())
}

fn forwards_running_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    let (rows, listening) = view.capture_forward_counts();
    if rows != 3 || listening != 1 {
        return Err(format!(
            "the forwarding page shows {rows} rules and {listening} listening, not 3 and 1"
        ));
    }
    Ok(())
}

fn home_empty_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Empty)?;
    if !view.book().hosts().is_empty() {
        return Err(format!(
            "the empty page holds {} saved host(s)",
            view.book().hosts().len()
        ));
    }
    Ok(())
}

fn home_hosts_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Populated)?;
    let listed = view.book().hosts().len();
    if listed != HOME_HOSTS {
        return Err(format!(
            "the hosts page lists {listed} host(s), not the {HOME_HOSTS} the fixture holds"
        ));
    }
    let recent = view.book().recent_hosts().len();
    if recent != 5 {
        return Err(format!(
            "the book has {recent} recent connection(s), not the five the fixture wrote"
        ));
    }
    Ok(())
}

/// The favourites place is on screen, with the three the fixture marked.
fn home_favorites_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    home_hosts_ready(view, cx)?;
    let favorites = view.book().favorite_hosts().len();
    if favorites != 3 {
        return Err(format!(
            "the book has {favorites} favourite(s), not the three the fixture marked"
        ));
    }
    let place = view.capture_host_view_id();
    if place != "hosts.view.favorites" {
        return Err(format!("the list shows {place}, not the favourites"));
    }
    Ok(())
}

fn terminal_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    on_page(view, Destination::LocalShells, SectionState::Populated)?;
    let Some(pane) = view.pane() else {
        return Err("the terminal destination has no pane on screen".to_owned());
    };
    let text = pane.read(cx).visible_text().join("\n");
    for needle in ["deploy@staging", "cargo test", "部署完成"] {
        if !text.contains(needle) {
            return Err(format!(
                "the terminal does not show {needle:?}, so its grid did not receive the fixture"
            ));
        }
    }
    Ok(())
}

/// Whether a settings page is on screen, and which one.
fn settings_page_is_on(view: &ShelluneView, wanted: (&str, &str)) -> Result<(), String> {
    if view.workspace().destination() != Destination::Settings {
        return Err("the settings page is not the destination on screen".to_owned());
    }
    let shown = view.settings_page_id();
    if shown != wanted {
        return Err(format!(
            "the settings page shows {}/{}, not {}/{}",
            shown.0, shown.1, wanted.0, wanted.1
        ));
    }
    Ok(())
}

fn settings_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    settings_page_is_on(view, ("appearance", "theme"))
}

fn settings_connection_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    settings_page_is_on(view, ("connection", "policy"))
}

/// The shortcut page is on screen, with the actions in force listed.
fn settings_keys_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    settings_page_is_on(view, ("keys", "keys"))?;
    let listed = view.keymap().matching("", view.workspace().locale()).len();
    if listed == 0 {
        return Err("the shortcut page lists no action".to_owned());
    }
    Ok(())
}

/// The shortcut page is on screen, narrowed to the combinations the query names.
fn settings_keys_search_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    settings_keys_ready(view, cx)?;
    let query = view.keys_query(cx);
    if query.is_empty() {
        return Err("the shortcut search holds no query".to_owned());
    }
    let locale = view.workspace().locale();
    // The page's own list, narrowed by the query it holds, against the whole catalogue.
    let listed = view.settings_key_rows(cx).len();
    let all = view.keymap().matching("", locale).len();
    if listed == 0 || listed >= all {
        return Err(format!(
            "the query {query:?} lists {listed} of {all} actions, so it does not narrow them"
        ));
    }
    Ok(())
}

/// The shortcut page is on screen, saying which two actions share a combination.
fn settings_keys_conflict_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    settings_keys_ready(view, cx)?;
    match view.keymap_problem() {
        Some(Problem::Conflict { .. }) => Ok(()),
        other => Err(format!(
            "the shortcut page reports {other:?} instead of a conflict"
        )),
    }
}

fn hold_splash(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_hold_splash(cx);
    Ok(())
}

fn show_catalog(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_show_catalog(cx);
    Ok(())
}

fn catalog_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if !view.catalog_shown() {
        return Err("the window is not drawing the design catalogue".to_owned());
    }
    Ok(())
}

fn splash_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if !view.starting() {
        return Err("the window is not on its start".to_owned());
    }
    Ok(())
}

fn palette_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if !view.palette_open() {
        return Err("the command palette is not open".to_owned());
    }
    Ok(())
}

fn snippets_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    if view.capture_overlay() != Some("snippets") {
        return Err(format!(
            "the open tool is {:?}, not the saved commands",
            view.capture_overlay()
        ));
    }
    terminal_ready(view, cx)
}

fn tabs_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    terminal_ready(view, cx)?;
    let count = view.workspace().layout().tabs().len();
    if count != 3 {
        return Err(format!(
            "the strip holds {count} tab(s), not the three the fixture opened"
        ));
    }
    let bells = view
        .workspace()
        .sessions()
        .items()
        .iter()
        .filter(|session| view.workspace().session_unread(session.id()).bell)
        .count();
    if bells != 1 {
        return Err(format!(
            "{bells} tab(s) carry a background bell, not the one the fixture rang"
        ));
    }
    Ok(())
}

fn tabs_overflow_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    terminal_ready(view, cx)?;
    let count = view.workspace().layout().tabs().len();
    if count != OVERFLOW_TABS {
        return Err(format!(
            "the strip holds {count} tab(s), not the {OVERFLOW_TABS} the fixture opened"
        ));
    }
    // The point of the shot is a strip that runs on past both ends; until it has been
    // laid out that way the picture would be of something else.
    match view.capture_tab_strip_ends() {
        (true, true) => Ok(()),
        ends => Err(format!(
            "the strip does not run on past both ends yet: {ends:?}"
        )),
    }
}

fn tabs_connecting_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    terminal_ready(view, cx)?;
    let connecting = view
        .workspace()
        .sessions()
        .items()
        .iter()
        .filter(|session| matches!(session.state(), SessionState::Connecting))
        .count();
    if connecting != 1 {
        return Err(format!(
            "{connecting} session(s) are connecting, not the one the fixture reconnected"
        ));
    }
    Ok(())
}

fn tabs_notices_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    tabs_connecting_ready(view, cx)?;
    if view.capture_update_stage() != "available" {
        return Err("no release is on offer, so the top row has no update notice".to_owned());
    }
    if !view.capture_sync_marked() {
        return Err("the sync is not marked as failed".to_owned());
    }
    Ok(())
}

fn split_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    on_page(view, Destination::LocalShells, SectionState::Populated)?;
    let panes = view.workspace().layout().leaves().len();
    if panes != 2 {
        return Err(format!(
            "the tab holds {panes} pane(s), not the two the fixture split into"
        ));
    }
    // The pane the shot is about is the one the split just made, and it holds its own
    // text: the two panes have to be told apart by what they show, not only by a frame.
    let Some(pane) = view.pane() else {
        return Err("the split has no pane on screen".to_owned());
    };
    let text = pane.read(cx).visible_text().join("\n");
    if !text.contains("systemctl status shellune") {
        return Err(
            "the active pane does not show its own transcript, so it is not the new pane"
                .to_owned(),
        );
    }
    Ok(())
}

fn paste_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    if view.capture_paste_open(cx) {
        Ok(())
    } else {
        Err("the paste confirmation is not open".to_owned())
    }
}

fn connection_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay() == Some("connect") {
        Ok(())
    } else {
        Err(format!(
            "the open overlay is {:?}, not the connection dialog",
            view.capture_overlay()
        ))
    }
}

fn tab_list_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.tab_list_open() {
        Ok(())
    } else {
        Err("the list of every tab is not open".to_owned())
    }
}

fn disconnected_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    let ended = view
        .workspace()
        .sessions()
        .items()
        .iter()
        .filter(|session| matches!(session.state(), SessionState::Disconnected(_)))
        .count();
    if ended != 1 {
        return Err(format!(
            "{ended} session(s) are disconnected, not the one the fixture lost"
        ));
    }
    Ok(())
}
// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Thousands of hosts (R18)
// ---------------------------------------------------------------------------

/// How many hosts the scale fixture holds: the number R18 asks the list to stay usable
/// at.
const SCALE_HOSTS: usize = 5_000;

/// The saved hosts of the scale fixture, derived from their position.
///
/// The identifiers are written rather than generated because the list draws a row from
/// the host's identifier: two runs of one shot have to draw the same list. Names,
/// addresses, users and groups come from the position for the same reason, and the
/// hierarchy is written as a group path rather than as a label with a separator in it.
fn scale_hosts(count: usize) -> Result<Vec<HostConfig>, String> {
    let mut hosts = Vec::with_capacity(count);
    for index in 0..count {
        let number = index + 1;
        let shape = crate::app::hosts::scale_host_shape(index);
        let record = serde_json::json!({
            "id": format!("00000000-0000-4000-8000-{number:012}"),
            "name": shape.name,
            "address": shape.address,
            "port": 22,
            "user": shape.user,
            "group_path": shape.group,
        });
        let host: HostConfig = serde_json::from_value(record).map_err(|error| {
            format!("the scale fixture host {number} is not a valid record: {error}")
        })?;
        hosts.push(host);
    }
    Ok(hosts)
}

/// Writes the scale fixture into the run's own root.
fn seed_scaled_hosts(root: &Path) -> Result<(), String> {
    write_hosts(root, scale_hosts(SCALE_HOSTS)?)
}

/// Writes a book of hosts, as a previous run would have left it.
fn write_hosts(root: &Path, hosts: Vec<HostConfig>) -> Result<(), String> {
    let mut book = shellune_storage::HostBook::new();
    for host in hosts {
        book.apply(&HostEdit::create(host))
            .map_err(|error| format!("the host fixture could not be built: {error}"))?;
    }
    HostStore::at(root)
        .save(&book)
        .map_err(|error| format!("the host fixture could not be written: {error}"))
}

/// A configuration file with eight hosts, three of which need a look.
///
/// The two that jump through an alias the file never defines are what the importer
/// reports as needing review, and the alive directives cover what it does with the
/// keepalive ones (R20): both applied, one interval that switches the probe off, one
/// count without an interval, and a TCPKeepAlive line that is named as not applied.
const IMPORT_CONFIG: &str = "\
Host web-01\n    HostName 192.0.2.11\n    User deploy\n    ServerAliveInterval 20\n    ServerAliveCountMax 4\n\
\n\
Host web-02\n    HostName 192.0.2.12\n    User deploy\n    IdentityFile ~/.ssh/id_ed25519\n    TCPKeepAlive no\n    ServerAliveInterval 60\n\
\n\
Host db-01\n    HostName 198.51.100.21\n    User postgres\n    ServerAliveInterval 0\n\
\n\
Host db-02\n    HostName 198.51.100.22\n    User postgres\n    ServerAliveCountMax 6\n\
\n\
Host edge-01\n    HostName 203.0.113.31\n    User ops\n\
\n\
Host edge-02\n    HostName 203.0.113.32\n    User ops\n\
\n\
Host through-bastion\n    HostName 203.0.113.40\n    User deploy\n    ProxyJump bastion.example.org\n\
\n\
Host through-unknown\n    HostName 203.0.113.41\n    User deploy\n    ProxyJump nowhere.example.org\n";

/// Writes that configuration into the run's own root.
fn seed_import_config(root: &Path) -> Result<(), String> {
    std::fs::write(root.join("config"), IMPORT_CONFIG)
        .map_err(|error| format!("the import fixture could not be written: {error}"))
}

/// Nothing to install: the list is what the fixture produced.
fn show_scale_hosts(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_home_section(crate::app::hosts::HomeSection::All, cx);
    Ok(())
}

/// One group of the sidebar's tree, shown in the list (R4, R18, ADR-3).
fn show_group_tree(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_home_section(
        crate::app::hosts::HomeSection::Group(
            shellune_core::GroupPath::parse("Prod")
                .map_err(|error| format!("the group fixture is not a path: {error}"))?,
        ),
        cx,
    );
    Ok(())
}

/// A selection of the current result, with the scope and the count it covers (R18).
fn show_selection(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    // The shot narrows the list first: the selection then covers a result smaller than
    // the book, which is exactly the difference the scope has to make visible.
    view.capture_group_filter("Prod", cx);
    view.capture_select_hosts(250, cx);
    Ok(())
}

/// The import panel on the fixture configuration (R18).
fn show_import(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_import_local("config", cx);
    Ok(())
}

/// Writes the global connection numbers a previous run would have saved (R20).
///
/// The page is drawn after a real read of this file, so the shot shows what a restart
/// shows rather than a value pushed into the view.
fn seed_connection_defaults(root: &Path) -> Result<(), String> {
    let defaults = shellune_core::ConnectionDefaults::new(60, 5, 45)
        .map_err(|error| format!("the connection fixture is not a valid policy: {error}"))?;
    PreferencesStore::at(root)
        .save(&Preferences {
            connection: Some(defaults),
            ..Default::default()
        })
        .map_err(|error| format!("the connection fixture could not be written: {error}"))
}

/// The connection page, with the numbers in force (R20).
fn show_settings_connection(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_item(view, window, cx, Item::Policy)
}

/// A saved host in the editor, with its keepalive choice (R20).
fn show_host_connection(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
    choice: ConnectionChoice,
) -> Result<(), String> {
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    view.capture_host_connection(&host, choice, cx);
    // The policy section is the last block of the form; a shot that claims to show it
    // has to be scrolled to it (R20).
    view.capture_scroll_host_form_to_bottom();
    Ok(())
}

fn show_host_connection_inherit(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_host_connection(view, window, cx, ConnectionChoice::Inherit)
}

fn show_host_connection_off(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_host_connection(view, window, cx, ConnectionChoice::Off)
}

fn show_host_connection_override(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_host_connection(view, window, cx, ConnectionChoice::Override)
}

/// The connection page is on screen, showing the numbers the preferences file held.
/// The backup a capture imports, written beside the stores of the run.
const BACKUP_FIXTURE: &str = "capture-backup.json";

/// Opens the export dialog over the data category, with the passwords included and one
/// passphrase typed into both fields (R5).
///
/// Nothing is exported: the shot is the dialog before the save dialog would open.
fn show_backup_export(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_item(view, window, cx, Item::ExportBackup)?;
    view.capture_backup_export(cx);
    Ok(())
}

fn backup_export_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    match view.backup_stage() {
        Some("export") => Ok(()),
        other => Err(format!("the export dialog is not open: {other:?}")),
    }
}

/// Writes the saved hosts, and beside them a backup made on a Mac: two of the same
/// hosts (one of them changed), two new ones - one naming a private key that is not on
/// this machine - and one group of its own (R6).
///
/// The document is built and written before the view exists; the preview the shot shows
/// is the real plan of applying it to the hosts on disk. Its time stamp is fixed, so the
/// line that says where the file came from reads the same on every run.
fn seed_backup_import(root: &Path) -> Result<(), String> {
    seed_hosts(root)?;
    let saved = saved_hosts()?;
    let mut book = shellune_storage::HostBook::new();
    let mut hosts: Vec<HostConfig> = saved.into_iter().take(2).collect();
    if let Some(changed) = hosts.get_mut(1) {
        changed.set_startup_command(Some("tmux attach || tmux"));
    }
    let mut mac = HostConfig::new("build-mac", "10.0.8.21", 22, "ci")
        .map_err(|error| format!("the backup fixture host is not valid: {error}"))?;
    mac.set_group(Some("构建"));
    mac.set_auth(Some(shellune_core::AuthMethodKind::KeyFile));
    mac.set_identity_file(Some("/Users/ci/.ssh/id_ed25519_build"));
    let mut runner = HostConfig::new("runner-02", "10.0.8.22", 22, "ci")
        .map_err(|error| format!("the backup fixture host is not valid: {error}"))?;
    runner.set_group(Some("构建"));
    hosts.push(mac);
    hosts.push(runner);
    for host in hosts {
        book.apply(&HostEdit::create(host))
            .map_err(|error| format!("the backup fixture could not be built: {error}"))?;
    }
    let local = shellune_storage::LocalState {
        hosts: book,
        ..shellune_storage::LocalState::default()
    };
    let mut options = shellune_storage::ExportOptions::new(shellune_storage::Platform::Macos);
    options.created_unix = 1_794_816_000;
    options.app_version = env!("CARGO_PKG_VERSION").to_owned();
    shellune_storage::ShelluneData::export(
        &local,
        &options,
        &shellune_storage::MemorySecrets::new(),
        None,
    )
    .map_err(|error| format!("the backup fixture could not be made: {error}"))?
    .document
    .write_to(&root.join(BACKUP_FIXTURE))
    .map_err(|error| format!("the backup fixture could not be written: {error}"))
}

/// Opens the import preview of the fixture backup over the data category (R6).
fn show_backup_import_preview(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_item(view, window, cx, Item::ImportBackup)?;
    let path = view.capture_data_root().join(BACKUP_FIXTURE);
    view.capture_backup_import(&path, cx);
    Ok(())
}

fn backup_import_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    match view.backup_stage() {
        Some("import.preview") => Ok(()),
        other => Err(format!("the import preview is not open: {other:?}")),
    }
}

/// Writes the saved hosts and sync settings for a server that is never reached: the
/// capture window is offline, so the page is drawn from these files alone (R7, R8).
///
/// `last` is what the state file says the last sync did; its time is fixed, so the
/// status line reads the same on every run.
fn seed_sync_with(root: &Path, last: shellune_sync::LastResult) -> Result<(), String> {
    seed_hosts(root)?;
    let mut config =
        shellune_sync::SyncConfig::new("https://dav.example.com/remote.php/dav/files/me/", "me");
    config.auto = true;
    shellune_sync::SyncConfigStore::at(root)
        .save(&config)
        .map_err(|error| format!("the sync settings could not be written: {error}"))?;
    let state = shellune_sync::SyncState {
        last_result: Some(last),
        last_time_unix: Some(1_794_816_000),
        last_success_unix: Some(1_794_816_000),
        ..shellune_sync::SyncState::default()
    };
    shellune_sync::SyncStateStore::at(root)
        .save(&state)
        .map_err(|error| format!("the sync state could not be written: {error}"))
}

fn seed_sync(root: &Path) -> Result<(), String> {
    seed_sync_with(root, shellune_sync::LastResult::Uploaded)
}

fn seed_sync_conflict(root: &Path) -> Result<(), String> {
    seed_sync_with(root, shellune_sync::LastResult::Conflict)
}

/// Opens the sync category over saved settings (R7, R8).
fn show_sync_settings(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_item(view, window, cx, Item::Sync)?;
    view.capture_sync_settings(cx);
    Ok(())
}

fn sync_settings_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    settings_page_is_on(view, ("sync", "sync"))
}

/// Opens the conflict question over the sync category, with a fixed report of a Mac's
/// cloud file; nothing is resolved (R10).
fn show_sync_conflict(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_item(view, window, cx, Item::Sync)?;
    view.capture_sync_conflict(cx);
    Ok(())
}

fn sync_conflict_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_sync_conflict_open() {
        Ok(())
    } else {
        Err("the sync conflict question is not open".to_owned())
    }
}

/// Opens the update card with a fixed newer release on offer, with its notes (R2). The
/// window has no updater, so nothing is sent and nothing can be downloaded.
fn show_update_available(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_settings_item(view, window, cx, Item::Update)?;
    view.capture_update_available(cx);
    Ok(())
}

fn update_available_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    settings_page_is_on(view, ("about", "update"))?;
    match view.capture_update_stage() {
        "available" => Ok(()),
        other => Err(format!(
            "the update card is {other}, not offering a release"
        )),
    }
}

/// Two local shells and a saved host are open when the install is asked for: the
/// question counts them (R3). None of them runs a process or a connection.
fn show_update_confirm(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    add_session(view, window, cx, SHOT_SHELL, TRANSCRIPT.as_bytes())?;
    add_session(
        view,
        window,
        cx,
        SHOT_SPLIT_SHELL,
        SPLIT_TRANSCRIPT.as_bytes(),
    )?;
    let host = saved_hosts()?
        .into_iter()
        .next()
        .ok_or_else(|| "the host fixture is empty".to_owned())?;
    view.attach_capture_remote_session(
        &host,
        b"$ uptime\r\n 09:20:11 up 12 days\r\n$ ",
        Some(window),
        cx,
    );
    view.capture_update_confirm(window, cx);
    Ok(())
}

fn update_confirm_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if !view.capture_update_confirm_open() {
        return Err("the question before the install is not open".to_owned());
    }
    match view.capture_update_confirm_lines() {
        0 => Err("the question lists no running session".to_owned()),
        _ => Ok(()),
    }
}

fn settings_connection_custom_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    settings_page_is_on(view, ("connection", "policy"))?;
    let defaults = view.connection_defaults();
    let shown = (
        defaults.keepalive_interval_secs(),
        defaults.keepalive_max_misses(),
        defaults.connect_timeout_secs(),
    );
    if shown != (60, 5, 45) {
        return Err(format!(
            "the page shows {shown:?}, not the (60, 5, 45) the file holds"
        ));
    }
    Ok(())
}

/// The host form is open and says one of the three keepalive choices (R20).
fn host_connection_ready_for(view: &ShelluneView, choice: &str) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Populated)?;
    match view.host_connection_choice() {
        Some(current) if current != choice => Err(format!(
            "the host form says {current}, not the {choice} the shot is about"
        )),
        Some(_) if view.capture_host_form_scrolled() => Ok(()),
        Some(_) => Err("the host form is still at its top; the policy section is below".to_owned()),
        None => Err("the host form is not open".to_owned()),
    }
}

fn host_connection_inherit_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    host_connection_ready_for(view, "inherit")
}

fn host_connection_off_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    host_connection_ready_for(view, "off")
}

fn host_connection_override_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    host_connection_ready_for(view, "override")
}

/// The list holds five thousand saved hosts and the page shows them.
fn many_hosts_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Populated)?;
    let listed = view.book().hosts().len();
    if listed != SCALE_HOSTS {
        return Err(format!(
            "the page holds {listed} host(s), not the {SCALE_HOSTS} the fixture wrote"
        ));
    }
    Ok(())
}

/// One group is shown, with the tree open in the sidebar.
fn group_tree_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    many_hosts_ready(view, _cx)?;
    let view_id = view.capture_host_view_id();
    if view_id != "hosts.view.group" {
        return Err(format!("the list is in {view_id}, not one group"));
    }
    let groups = view.capture_group_tree_len();
    if groups < 4 {
        return Err(format!(
            "the tree holds {groups} group(s), so the shot does not show a hierarchy"
        ));
    }
    Ok(())
}

/// The shot holds a selection and says what it covers.
fn selection_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    many_hosts_ready(view, _cx)?;
    let (count, scope) = view.capture_selection();
    if count != 250 {
        return Err(format!("the selection holds {count} host(s), not 250"));
    }
    if scope != "hosts.scope.current" {
        return Err(format!(
            "the selection covers {scope} rather than the current result"
        ));
    }
    Ok(())
}

/// The import panel read the fixture and offers its hosts.
fn import_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    let offered = view
        .capture_import_count()
        .ok_or_else(|| "the import panel holds no preview".to_owned())?;
    if offered != 8 {
        return Err(format!(
            "the preview offers {offered} host(s), not the eight the fixture describes"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Menus (R6)
// ---------------------------------------------------------------------------

/// Where a menu shot asks its menu to open, in the window's logical pixels. The menu
/// itself is kept inside the window by the same rules a right click uses, so the
/// smallest window moves it rather than cutting it.
const MENU_AT: (f32, f32) = (260.0, 120.0);

fn menu_point() -> gpui::Point<gpui::Pixels> {
    gpui::point(gpui::px(MENU_AT.0), gpui::px(MENU_AT.1))
}

/// A saved host's terminal ended the way a lost connection ends, with its tab's menu
/// open, so the menu carries the reconnect entry (R6, R8).
fn show_tab_menu(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_disconnected(view, window, cx)?;
    view.capture_open_tab_menu(gpui::point(gpui::px(220.0), gpui::px(40.0)), cx)
}

fn tab_menu_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    disconnected_ready(view, cx)?;
    menu_is(view, "tab.menu")
}

/// The hosts page with the first saved host's right-click menu open (R6).
fn show_host_menu(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_open_host_menu(gpui::point(gpui::px(560.0), gpui::px(330.0)), cx)
}

fn host_menu_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Populated)?;
    menu_is(view, "host.menu")
}

/// The file view with the right-click menu of one server file open (R6).
fn show_files_menu(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_files(view, window, cx)?;
    let id = view
        .workspace()
        .layout()
        .active_session()
        .ok_or_else(|| "the file view has no session on screen".to_owned())?;
    view.capture_open_files_menu(
        id,
        "构建 产物.tar.zst",
        gpui::point(gpui::px(900.0), gpui::px(330.0)),
        cx,
    );
    Ok(())
}

fn files_menu_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    files_ready(view, cx)?;
    menu_is(view, "files.menu")
}

/// The terminal with its right-click menu open over the grid (R6).
fn show_terminal_menu(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_terminal(view, window, cx)?;
    let pane = view
        .pane()
        .cloned()
        .ok_or_else(|| "the terminal destination has no pane on screen".to_owned())?;
    pane.update(cx, |pane, cx| pane.capture_open_menu(menu_point(), cx));
    Ok(())
}

fn terminal_menu_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    terminal_ready(view, cx)?;
    let open = view
        .pane()
        .is_some_and(|pane| pane.read(cx).capture_menu_open());
    if !open {
        return Err("the terminal's menu is not open".to_owned());
    }
    Ok(())
}

/// The hosts page with the header's shell menu open under its control (R6).
fn show_shell_menu(
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    show_terminal(view, window, cx)?;
    view.capture_open_shell_menu(cx);
    Ok(())
}

fn shell_menu_ready(view: &ShelluneView, cx: &App) -> Result<(), String> {
    terminal_ready(view, cx)?;
    menu_is(view, "shell.picker")
}

fn menu_is(view: &ShelluneView, wanted: &str) -> Result<(), String> {
    match view.capture_menu() {
        Some(open) if open == wanted => Ok(()),
        other => Err(format!("the open menu is {other:?}, not {wanted}")),
    }
}

/// Every scenario, in the order the gallery lists them.
///
/// A later task adds its own entry here (the merged header, the host list, the batch
/// panel); it does not edit the shots of the scenarios already listed.
pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "home-empty",
        title: "首页 · 没有保存的主机",
        category: Category::Home,
        fixture: "an empty data directory",
        evidence: "shows the empty home page: the sidebar, the toolbar with its one search and the \
                   welcome in the list's place with its four ways to start; says nothing about a \
                   host list with items",
        quick: false,
        variants: &[Variant::dark().smallest(), Variant::light().chinese()],
        start: || page(Destination::Hosts),
        prepare: nothing,
        install: no_state,
        ready: home_empty_ready,
    },
    Scenario {
        id: "home-hosts",
        title: "首页 · 侧栏、工具条与 24 台主机的列表",
        category: Category::Home,
        fixture: "24 saved hosts in nested groups and without one, five of them with a connection \
                  history stamped relative to the run and three marked as favourites, seeded on \
                  disk before the view starts",
        evidence: "shows the sidebar with its places and the group tree, the toolbar with the \
                   title, count, search, filter, more and new host, and the rows with avatar, \
                   address, group tag, time and state; the history was written, not earned, so \
                   no host was ever connected",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::light().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_home,
        install: no_state,
        ready: home_hosts_ready,
    },
    Scenario {
        id: "home-favorites",
        title: "首页 · 侧栏的收藏",
        category: Category::Home,
        fixture: "the same 24 hosts and history, with edge-03, web-01 and lab-07 marked as \
                  favourites in hosts.json and the favourites place chosen",
        evidence: "shows the favourites place chosen in the sidebar, its title and count, and the \
                   filled star on each row; marking and clearing a favourite is proved by the GPUI \
                   tests, not by this frame",
        quick: true,
        variants: &[Variant::dark().chinese(), Variant::light().comfortable()],
        start: || page(Destination::Hosts),
        prepare: seed_home,
        install: show_home_favorites,
        ready: home_favorites_ready,
    },
    Scenario {
        id: "home-direct",
        title: "首页 · 搜索里输入 user@host",
        category: Category::Home,
        fixture: "the same 24 hosts, with deploy@192.0.2.24 typed into the search; api-02 is saved \
                  at that address",
        evidence: "shows the \"connect to …\" row on top with the Enter key cap and the saved host \
                   the address also finds under it; nothing was opened and no host was dialled",
        quick: false,
        variants: &[
            Variant::dark().chinese().smallest(),
            Variant::light().comfortable(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_home,
        install: show_direct,
        ready: direct_ready,
    },
    Scenario {
        id: "home-narrow",
        title: "首页 · 窄窗口：侧栏收成图标",
        category: Category::Home,
        fixture: "the same 24 hosts, drawn narrower than the 900 threshold where the sidebar \
                  starts folded",
        evidence: "the 860x640 and 720x480 shots show the sidebar as a rail of icons, the toolbar \
                   wrapping its controls and the rows dropping their tags and times, and at 200% \
                   the row tools giving way before the names; the 1120x720 shots are the wide page \
                   beside them, sidebar open",
        quick: false,
        variants: &[
            Variant::dark().sized(860.0, 640.0),
            Variant::light().sized(860.0, 640.0).chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_home,
        install: no_state,
        ready: home_hosts_ready,
    },
    Scenario {
        id: "home-collapsed",
        title: "首页 · 宽窗口里收起的侧栏",
        category: Category::Home,
        fixture: "the same 24 hosts on a wide window, with the sidebar folded by its button",
        evidence: "shows the 56-wide rail of icons with the chosen place marked and the list taking \
                   the room the words gave up; the rail's tooltips are not part of the frame",
        quick: false,
        variants: &[Variant::dark().comfortable(), Variant::light().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_home,
        install: show_home_folded,
        ready: home_folded_ready,
    },
    Scenario {
        id: "hosts-5000",
        title: "主机 · 五千台的可扫描列表",
        category: Category::Home,
        fixture: "five thousand saved hosts in four groups, written to the run's own root before \
                  the view starts",
        evidence: "shows the list this build draws and the filters above it; the list is asked \
                   for the rows it shows, so the shot cannot show how many elements exist. No host \
                   was connected, so no connection state is part of it",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_scaled_hosts,
        install: show_scale_hosts,
        ready: many_hosts_ready,
    },
    Scenario {
        id: "hosts-groups",
        title: "主机 · 侧栏分组树与一个分组",
        category: Category::Home,
        fixture: "the same five thousand hosts, grouped Prod/Web, Prod/DB, Prod/Edge and \
                  Staging, with the Prod group chosen in the sidebar",
        evidence: "shows the nested group tree in the sidebar with Prod chosen, the page titled \
                   by the group and the flat list of its hosts with their group tags; opening and \
                   closing a group is not part of the shot, and no group was created or moved by it",
        quick: true,
        variants: &[
            Variant::dark().chinese().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_scaled_hosts,
        install: show_group_tree,
        ready: group_tree_ready,
    },
    Scenario {
        id: "hosts-selection",
        title: "主机 · 批量选择与范围",
        category: Category::Home,
        fixture: "the same five thousand hosts with 250 of the current result selected",
        evidence: "shows the count and the scope of a selection, which is what a batch target \
                   would use; nothing was moved and no command was prepared",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_scaled_hosts,
        install: show_selection,
        ready: selection_ready,
    },
    Scenario {
        id: "hosts-import",
        title: "主机 · 导入结果的可扫描列表",
        category: Category::Home,
        fixture: "a configuration file with eight hosts: two jumping through an alias the file \
                  never defines, one turning keepalive off, one setting only a probe count and one \
                  whose TCPKeepAlive is not applied",
        evidence: "shows the preview, its limits and a row per host with the outcome the importer \
                   produced, including the keepalive it would apply; nothing was saved and no host \
                   was dialled",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_import_config,
        install: show_import,
        ready: import_ready,
    },
    Scenario {
        id: "hosts-connection-inherit",
        title: "主机表单 · 继承全局保活",
        category: Category::Home,
        fixture: "six saved hosts, none of them stating its own keepalive; the editor open on the \
                  first",
        evidence: "shows the three-way choice on inherit and the numbers it inherits; nothing was \
                   saved and no connection was made",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_host_connection_inherit,
        ready: host_connection_inherit_ready,
    },
    Scenario {
        id: "hosts-connection-off",
        title: "主机表单 · 关闭这一台的保活",
        category: Category::Home,
        fixture: "the same hosts, with the editor on the off choice",
        evidence: "shows what switching keepalive off for one host means and that the connect \
                   timeout still follows the default; nothing was saved",
        quick: false,
        variants: &[Variant::dark().chinese().smallest()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_host_connection_off,
        ready: host_connection_off_ready,
    },
    Scenario {
        id: "hosts-connection-override",
        title: "主机表单 · 覆盖保活与超时",
        category: Category::Home,
        fixture: "the same hosts, with the editor on the override choice and the three numbers it \
                  would save",
        evidence: "shows the three numbers, the detection window they add up to and that they apply \
                   to this host alone; no value was typed and nothing was saved",
        quick: false,
        variants: &[
            Variant::light().chinese().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_host_connection_override,
        ready: host_connection_override_ready,
    },
    Scenario {
        id: "serial-session",
        title: "串口 · 会话与端口错误",
        category: Category::Terminal,
        fixture: "two display-only serial panes: one open on COM3 with a fixed transcript, one that could not open COM7 with a port list holding COM3; no port was opened",
        evidence: "proves what the serial strip, the send habits and the failure card say in these states; proves nothing about a device, a driver, hardware flow control or an unplug, none of which this machine has",
        quick: true,
        variants: &[
            Variant::dark().smallest(),
            Variant::dark().comfortable(),
            Variant::dark().chinese(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_serial,
        ready: serial_ready,
    },
    Scenario {
        id: "hosts-connections",
        title: "主机 · 桌面与串口记录及其连接动作",
        category: Category::Home,
        fixture: "six saved hosts plus one RDP, one VNC and one serial record, written before the view starts",
        evidence: "shows that the records of other protocols are listed beside the hosts with the same labelled connect button and no selection box; nothing was dialled and no session exists",
        quick: true,
        variants: &[Variant::dark().smallest(), Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_connections,
        install: no_state,
        ready: connections_listed_ready,
    },
    Scenario {
        id: "home-no-match",
        title: "首页 · 搜索无结果与清除",
        category: Category::Home,
        fixture: "the 24 saved hosts with the phrase no-such-host typed into the search field",
        evidence: "shows the reason, the current condition and the press that drops it over a real narrowed list; proves nothing about a search across a live server list",
        quick: true,
        variants: &[
            Variant::dark().smallest(),
            Variant::dark().chinese(),
            Variant::light().comfortable(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_home,
        install: show_no_match,
        ready: no_match_ready,
    },
    Scenario {
        id: "hosts-read-failed",
        title: "主机 · 主机文件读不出来",
        category: Category::Home,
        fixture: "a hosts file holding text that is not a host book, written before the view starts",
        evidence: "shows that an unreadable store is not drawn as an empty list, and which reason and retry it offers; proves nothing about a real disk failure",
        quick: false,
        variants: &[Variant::dark().smallest(), Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: damage_hosts,
        install: no_state,
        ready: hosts_failed_ready,
    },
    Scenario {
        id: "desktop-rdp",
        title: "远程桌面 · RDP 画面与控件",
        category: Category::Desktop,
        fixture: "a display-only RDP surface mounted on a scripted link: four flat regions and a connected state; no server was dialled",
        evidence: "shows the tab, the control strip and the zoom the window draws around a picture; proves \
                   nothing about a real RDP server, its frames, its input or its clipboard",
        quick: true,
        variants: &[
            Variant::dark().smallest(),
            Variant::dark().comfortable(),
            Variant::dark().chinese(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_desktop_rdp,
        ready: desktop_live_ready,
    },
    Scenario {
        id: "desktop-vnc-failed",
        title: "远程桌面 · VNC 认证失败",
        category: Category::Desktop,
        fixture: "a display-only VNC surface whose scripted link reports an authentication failure; no server was dialled",
        evidence: "shows the card a refused sign-in leaves and the step it stopped on; proves nothing about a \
                   real VNC server's handshake or security types",
        quick: false,
        variants: &[Variant::dark().smallest(), Variant::dark().chinese()],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_desktop_vnc_failed,
        ready: desktop_failed_ready,
    },
    Scenario {
        id: "desktop-send-problem",
        title: "远程桌面 · 输入未送达的提示条",
        category: Category::Desktop,
        fixture: "a live display-only RDP surface with the send warning the pane draws after a send fails; no server was dialled",
        evidence: "shows where the warning sits over a picture and that it does not replace it; proves nothing \
                   about a real input path",
        quick: false,
        variants: &[Variant::dark().comfortable(), Variant::dark().chinese()],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_desktop_send_problem,
        ready: desktop_live_ready,
    },
    Scenario {
        id: "desktop-notices",
        title: "远程桌面 · 剪贴板与文件夹映射提示",
        category: Category::Desktop,
        fixture: "a live display-only RDP surface whose scripted link reported a mapped folder that does \
                  not exist and a clipboard the server refused; no server was dialled",
        evidence: "shows the notices over the picture, each said once, and the way to dismiss them; proves \
                   nothing about a real server's clipboard channel or drive redirection",
        quick: false,
        variants: &[
            Variant::dark().comfortable(),
            Variant::dark().chinese(),
            Variant::light().smallest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_desktop_notices,
        ready: desktop_live_ready,
    },
    Scenario {
        id: "terminal-local",
        title: "本地终端 · 会话与标签",
        category: Category::Terminal,
        fixture: "one display-only session fed a fixed transcript; no PTY or shell ran",
        evidence: "proves terminal layout, colour and text rendering, and the bar's three \
                   groups - where the session is, its own controls, the terminal's tools; at \
                   720x480 and 200% the bar follows the text size and folds the tools it has \
                   no room for into its menu; proves nothing about a live shell, its \
                   environment or its keystrokes",
        quick: true,
        variants: &[
            Variant::dark().smallest(),
            Variant::dark().comfortable(),
            Variant::dark().chinese(),
            Variant::light().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_terminal,
        ready: terminal_ready,
    },
    Scenario {
        id: "terminal-search",
        title: "终端 · 搜索打开",
        category: Category::Terminal,
        fixture: "one display-only session fed a fixed transcript, with the find bar open on a query",
        evidence: "shows the find bar, its query and the match count over the real grid; nothing was \
                   searched on a server",
        quick: false,
        variants: &[
            Variant::dark().comfortable(),
            Variant::light().comfortable(),
            Variant::dark().chinese(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_terminal_search,
        ready: terminal_search_ready,
    },
    Scenario {
        id: "connect-failed",
        title: "连接 · 认证失败",
        category: Category::Overlay,
        fixture: "a saved host whose connection dialog reports a refused sign-in; nothing was dialled",
        evidence: "shows the failure, its reason and the way back; no transport, DNS lookup or \
                   authentication is part of it",
        quick: false,
        variants: &[
            Variant::dark().comfortable(),
            Variant::light().comfortable(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_connection_failed,
        ready: connection_failed_ready,
    },
    Scenario {
        id: "confirm-delete-host",
        title: "确认 · 删除主机",
        category: Category::Home,
        fixture: "six saved hosts and the question shown before one of them is removed",
        evidence: "shows the destructive question, the host it names and the way out; nothing was \
                   deleted and no connection was made",
        quick: false,
        variants: &[
            Variant::dark().comfortable(),
            Variant::light().comfortable(),
            Variant::dark().chinese(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_delete_question,
        ready: delete_question_ready,
    },
    Scenario {
        id: "host-new",
        title: "对话框 · 新建主机",
        category: Category::Overlay,
        fixture: "six saved hosts and the empty form a new host starts from",
        evidence: "shows the shared dialog: header, scrolling form and the footer's action order; \
                   nothing was saved and no connection was made",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::light().smallest().largest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_new_host,
        ready: new_host_ready,
    },
    Scenario {
        id: "host-new-jumps",
        title: "对话框 · 新建主机 · 跳板主机选择",
        category: Category::Overlay,
        fixture: "twenty-four saved hosts; the empty form for a new host with its advanced section \
                  open, two of them chosen as jump hosts and the picker open",
        evidence: "shows the ordered jump cards, the add button and the searchable picker with a \
                   bounded, scrolling list above the ProxyCommand field; nothing was saved and no \
                   connection was made",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::light().smallest().largest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_jump_hosts,
        install: show_new_host_jumps,
        ready: new_host_jumps_ready,
    },
    Scenario {
        id: "connection-rdp-edit",
        title: "对话框 · 编辑 RDP 连接 · 剪贴板与映射文件夹",
        category: Category::Overlay,
        fixture: "one saved RDP connection that maps a local folder and one saved VNC connection, \
                  written before the view starts; the RDP connection's form is open",
        evidence: "shows the clipboard switch, the mapped-folder field with its browse button and its \
                   note in the RDP form; nothing was saved, dialled or mapped",
        quick: false,
        variants: &[Variant::dark().comfortable(), Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_sharing_connections,
        install: show_rdp_editor,
        ready: rdp_editor_ready,
    },
    Scenario {
        id: "connection-vnc-edit",
        title: "对话框 · 编辑 VNC 连接 · 剪贴板开关",
        category: Category::Overlay,
        fixture: "one saved RDP connection and one saved VNC connection with its clipboard turned off, \
                  written before the view starts; the VNC connection's form is open",
        evidence: "shows the clipboard switch, off, in the VNC form and that the form has no folder \
                   field; nothing was saved or dialled",
        quick: false,
        variants: &[Variant::dark().comfortable(), Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_sharing_connections,
        install: show_vnc_editor,
        ready: vnc_editor_ready,
    },
    Scenario {
        id: "close-question",
        title: "对话框 · 关闭前询问",
        category: Category::Overlay,
        fixture: "a remote session with one transfer the queue holds, and its tab's close asked for",
        evidence: "shows the question, the task it would stop and the way out; nothing was closed, \
                   cancelled or transferred",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_close_question,
        ready: close_question_ready,
    },
    Scenario {
        id: "notification",
        title: "通知 · 保存成功",
        category: Category::Overlay,
        fixture: "the local terminal with one saved notification over it",
        evidence: "shows the notification stack and its wording; nothing was saved by the shot",
        quick: false,
        variants: &[
            Variant::dark().comfortable(),
            Variant::light().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_notification,
        ready: notification_ready,
    },
    Scenario {
        id: "terminal-tabs",
        title: "终端 · 浏览器式标签与后台提醒",
        category: Category::Terminal,
        fixture: "two display-only shells and one saved host's terminal, the second tab \
                  carrying a background bell",
        evidence: "proves the browser-style strip: the current tab joins the page under it, \
                   each tab is a state dot, an icon, a cut title and a close button, and \
                   another tab's bell is marked while it is not the one on screen; no shell, \
                   SSH session or bell from a real program is part of it",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest(),
            Variant::light().comfortable(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_tabs,
        ready: tabs_ready,
    },
    Scenario {
        id: "tabs-overflow",
        title: "终端 · 标签溢出横向滚动",
        category: Category::Terminal,
        fixture: "fourteen display-only shells in one strip, a tab in the middle current, \
                  one of them with a long name",
        evidence: "proves a strip with more tabs than room scrolls instead of clipping: both ends \
                   fade and carry a scroll button, the current tab is in view, and the window \
                   buttons keep their place; no shell is running behind any tab",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest(),
            Variant::dark().comfortable(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_tabs_overflow,
        ready: tabs_overflow_ready,
    },
    Scenario {
        id: "tabs-connecting",
        title: "终端 · 标签连接中与长标题",
        category: Category::Terminal,
        fixture: "a display-only shell with a long name and a saved host's tab put back into \
                  connecting, the way a reconnect starts",
        evidence: "proves a connecting tab says so with its dot and keeps its width, and a long \
                   name is cut inside its own tab; at 720x480 and 200% the row is compact - \
                   the hosts tab is its icon and the current tab is shown alone, whole and \
                   cut with an ellipsis, the connecting one in the list of every tab; the \
                   capture runs with reduced motion, so the dot's breath is not in the \
                   picture, and no connection is being made",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest(),
            Variant::light().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_tabs_connecting,
        ready: tabs_connecting_ready,
    },
    Scenario {
        id: "tabs-notices",
        title: "终端 · 标题栏提示与当前标签",
        category: Category::Terminal,
        fixture: "the tabs-connecting fixture with a fixed newer release on offer and the \
                  last sync marked as failed; the window has no updater and is offline",
        evidence: "proves the top row's notices never take the current tab's room: with room \
                   they are words, in a 720x480 window they are icons in their tone, and in \
                   the compact row of 720x480 at 200% they fold into the settings button as \
                   a mark, the current tab still whole; nothing was sent or installed",
        quick: true,
        variants: &[
            Variant::dark().smallest(),
            Variant::light().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_tabs_notices,
        ready: tabs_notices_ready,
    },
    Scenario {
        id: "terminal-split",
        title: "终端 · 左右分屏",
        category: Category::Terminal,
        fixture: "one session split left and right, the second pane holding a second \
                  display-only session",
        evidence: "proves a split marks its panes and its active pane without repeating the \
                   session, and that each pane's bar folds the tools its width has no room \
                   for into its menu instead of running past the pane; the second pane has \
                   no process behind it",
        quick: true,
        variants: &[
            Variant::dark().comfortable().chinese(),
            Variant::dark().smallest(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_split,
        ready: split_ready,
    },
    Scenario {
        id: "terminal-disconnected",
        title: "终端 · 连接断开",
        category: Category::Terminal,
        fixture: "a saved host's display-only terminal, ended the way a lost connection ends",
        evidence: "proves the state, the target and the next step are a card centred over the \
                   last screen, which keeps its size and stays legible behind it, and that \
                   the card and the bar fit 720x480 at 200%; the capture runs with reduced \
                   motion, so the card's entrance is not in the picture; no connection was \
                   ever made, so no transport behaviour is shown",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::light().comfortable(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_disconnected,
        ready: disconnected_ready,
    },
    Scenario {
        id: "files-dual",
        title: "文件 · 本机与服务器双栏",
        category: Category::Terminal,
        fixture: "both file sources answered from fixture listings; nothing was read from disk or a server",
        evidence: "shows the two columns, their own paths, the actions each row offers and the direction \
                   between them; no directory was really listed, so no permission or server behaviour is shown",
        quick: true,
        variants: &[
            Variant::dark().chinese().comfortable(),
            Variant::dark().smallest(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_files,
        ready: files_ready,
    },
    Scenario {
        id: "files-marked",
        title: "文件 · 多选与批量动作",
        category: Category::Terminal,
        fixture: "both columns answered from fixture listings, with two rows of the server's \
                  column marked by the build itself",
        evidence: "shows the marks, the batch bar and what each action covers; nothing was \
                   transferred or deleted, and no server answered",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_marked_files,
        ready: marked_files_ready,
    },
    Scenario {
        id: "files-server-failed",
        title: "文件 · 远端失败只影响远端",
        category: Category::Terminal,
        fixture: "the server's column answered with a refusal and this computer's column with \
                  a listing, both from fixtures",
        evidence: "shows the reason and the recovery entries of the server's column beside \
                   this computer's unchanged list; no permission or disconnection was real",
        quick: false,
        variants: &[Variant::dark().chinese().smallest()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_isolated_failure,
        ready: isolated_failure_ready,
    },
    Scenario {
        id: "files-split",
        title: "文件 · 同屏终端与文件",
        category: Category::Terminal,
        fixture: "a window wide enough to dock both, with the terminal's transcript and both \
                  fixture listings",
        evidence: "shows the terminal and the file view in one row, which is what a wide \
                   window draws; the transcript is a fixture, not a live shell",
        quick: false,
        variants: &[Variant::dark().chinese().sized(1680.0, 940.0)],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_files,
        ready: files_ready,
    },
    Scenario {
        id: "files-empty",
        title: "文件 · 空目录",
        category: Category::Terminal,
        fixture: "a remote directory with no entries, answered from a fixture",
        evidence: "shows what an empty directory says and the actions still offered beside it; nothing \
                   was listed on a server",
        quick: false,
        variants: &[Variant::dark().chinese().smallest()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_empty_files,
        ready: empty_files_ready,
    },
    Scenario {
        id: "transfers-running",
        title: "传输 · 进行中与速率",
        category: Category::Overlay,
        fixture: "one transfer in the strip, started and progressed through the queue's own events",
        evidence: "shows the share, the measured speed and the time left of a task this build holds; \
                   no byte was moved and no connection took part",
        quick: true,
        variants: &[
            Variant::dark().chinese().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_running_transfer,
        ready: running_transfer_ready,
    },
    Scenario {
        id: "transfers-failed",
        title: "传输 · 失败与重试",
        category: Category::Overlay,
        fixture: "one transfer failed the way a lost connection stops it: it left a partial file",
        evidence: "shows the reason, the partial file and the retry actions of a task the queue really \
                   holds; no transfer was attempted",
        quick: true,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_failed_transfer,
        ready: failed_transfer_ready,
    },
    Scenario {
        id: "forwards-empty",
        title: "转发 · 尚未保存规则",
        category: Category::Overlay,
        fixture: "one saved host and an empty rule book",
        evidence: "shows the empty forwarding page and how a tunnel is created; nothing was bound or \
                   dialled",
        quick: false,
        variants: &[Variant::dark().chinese().smallest()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_forwards_empty,
        ready: forwards_empty_ready,
    },
    Scenario {
        id: "forwards-running",
        title: "转发 · 三种类型与运行状态",
        category: Category::Overlay,
        fixture: "three saved rules - local, remote and SOCKS - with the first marked as listening",
        evidence: "shows the kind, the host each rule goes through, which side each endpoint is on and \
                   the state of every row; no socket was bound and no connection exists",
        quick: true,
        variants: &[
            Variant::dark().chinese().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_forwards_running,
        ready: forwards_running_ready,
    },
    Scenario {
        id: "settings",
        title: "设置 · 分类导航与外观",
        category: Category::Settings,
        fixture: "default preferences; nothing is saved",
        evidence: "shows the category and item navigation and the one page it opens; the values \
                   are this build's defaults and no preference was written",
        quick: true,
        variants: &[
            Variant::dark().smallest(),
            Variant::dark().chinese().comfortable(),
        ],
        start: || page(Destination::Settings),
        prepare: nothing,
        install: no_state,
        ready: settings_ready,
    },
    Scenario {
        id: "settings-connection",
        title: "设置 · 连接分类的保活与超时",
        category: Category::Settings,
        fixture: "default preferences; the keepalive and timeout numbers nothing has changed",
        evidence: "shows the switch, the three numbers and the sentence saying how long a silent \
                   link takes to be noticed; nothing was dialled and the numbers are this build's \
                   defaults",
        quick: false,
        variants: &[Variant::dark().chinese().smallest()],
        start: || page(Destination::Settings),
        prepare: nothing,
        install: show_settings_connection,
        ready: settings_connection_ready,
    },
    Scenario {
        id: "settings-connection-custom",
        title: "设置 · 保存过的保活默认值",
        category: Category::Settings,
        fixture: "a preferences file that already holds 60 s, 5 probes and a 45 s timeout",
        evidence: "shows the page as a restart reads it back from the file; the numbers were \
                   written before the window started, so the shot proves the file reaches the \
                   fields, not that a keystroke did",
        quick: false,
        variants: &[Variant::light().chinese(), Variant::dark().comfortable()],
        start: || page(Destination::Settings),
        prepare: seed_connection_defaults,
        install: show_settings_connection,
        ready: settings_connection_custom_ready,
    },
    Scenario {
        id: "settings-keys",
        title: "设置 · 快捷键子页",
        category: Category::Settings,
        fixture: "default preferences; every action at its default binding",
        evidence: "shows the search, the scope filter and the bindings in force on the shortcut \
                   page alone; pressing a control is not part of the shot",
        quick: true,
        variants: &[
            Variant::dark().comfortable(),
            Variant::dark().chinese().smallest(),
        ],
        start: || page(Destination::Settings),
        prepare: nothing,
        install: show_settings_keys,
        ready: settings_keys_ready,
    },
    Scenario {
        id: "settings-keys-search",
        title: "设置 · 快捷键按组合键筛选",
        category: Category::Settings,
        fixture: "default preferences, with ctrl in the shortcut search",
        evidence: "shows the shortcut page narrowed to the actions whose keys contain the query, \
                   in both languages; the query was typed, no binding was changed",
        quick: false,
        variants: &[
            Variant::dark().chinese().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Settings),
        prepare: nothing,
        install: show_settings_keys_search,
        ready: settings_keys_search_ready,
    },
    Scenario {
        id: "settings-keys-conflict",
        title: "设置 · 快捷键冲突说明",
        category: Category::Settings,
        fixture: "a saved key map that binds two actions to one combination",
        evidence: "shows the shortcut page naming the two actions that collide and that the \
                   defaults are in force instead; no binding was changed by the shot",
        quick: false,
        variants: &[
            Variant::dark().chinese().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Settings),
        prepare: seed_conflicting_keymap,
        install: show_settings_keys,
        ready: settings_keys_conflict_ready,
    },
    Scenario {
        id: "settings-backup-export",
        title: "设置 · 数据：导出备份（含密码与口令）",
        category: Category::Settings,
        fixture: "default data; the export dialog opened with the passwords ticked and one \
                  passphrase typed into both fields",
        evidence: "shows the checkbox, the two masked passphrase fields and the export button \
                   this build draws; nothing was exported, and the passphrase checks and the \
                   file written are proved by the GPUI tests, not by this frame",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::Settings),
        prepare: nothing,
        install: show_backup_export,
        ready: backup_export_ready,
    },
    Scenario {
        id: "settings-backup-import-preview",
        title: "设置 · 数据：导入备份的预览",
        category: Category::Settings,
        fixture: "six saved hosts, and a backup made on macOS holding two of them (one \
                  changed), two new hosts in a group of their own and a private key path \
                  that is not on this machine",
        evidence: "shows the counts, the source line, the missing key and the merge/replace \
                   choice of a real plan against the hosts on disk; nothing was applied, and \
                   applying, replacing and refusing are proved by the GPUI tests",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().chinese().smallest(),
        ],
        start: || page(Destination::Settings),
        prepare: seed_backup_import,
        install: show_backup_import_preview,
        ready: backup_import_ready,
    },
    Scenario {
        id: "settings-sync",
        title: "设置 · 同步（WebDAV）",
        category: Category::Settings,
        fixture: "six saved hosts, sync settings for a server that is never reached and a \
                  state file saying the last sync uploaded at a fixed time",
        evidence: "shows the server fields, the automatic and password switches, the actions \
                   and the status line this build draws; the window is offline, so nothing \
                   was sent, and saving, testing, uploading and pulling are proved by the \
                   GPUI tests against the in-process WebDAV server",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::Settings),
        prepare: seed_sync,
        install: show_sync_settings,
        ready: sync_settings_ready,
    },
    Scenario {
        id: "sync-conflict",
        title: "浮层 · 同步冲突",
        category: Category::Overlay,
        fixture: "the sync fixture with a state file saying a conflict waits, and a fixed \
                  report of a cloud file written on macOS",
        evidence: "shows the conflict question, where the cloud data came from and the \
                   three answers, plus the conflict marker in the top bar; nothing was \
                   resolved, and each answer is proved by the GPUI tests",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().chinese().smallest(),
        ],
        start: || page(Destination::Settings),
        prepare: seed_sync_conflict,
        install: show_sync_conflict,
        ready: sync_conflict_ready,
    },
    Scenario {
        id: "settings-update-available",
        title: "设置 · 关于与更新（发现新版本）",
        category: Category::Settings,
        fixture: "a fixed release one minor version above this build, with four lines of \
                  notes and a signed portable file listed; the window has no updater",
        evidence: "shows the automatic-check switch, the check button and the card of a \
                   release on offer: version, date, notes, download and release page, plus \
                   the new-version chip in the top bar; nothing was sent, and checking, \
                   downloading, verifying and installing are proved by the GPUI tests \
                   against a loopback release server",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::Settings),
        prepare: seed_hosts,
        install: show_update_available,
        ready: update_available_ready,
    },
    Scenario {
        id: "update-confirm-sessions",
        title: "浮层 · 安装更新前确认活动会话",
        category: Category::Overlay,
        fixture: "two local shells and a saved host on display-only panes, and the release \
                  fixture of settings-update-available",
        evidence: "shows the question asked before an install while sessions run and the \
                   counts it lists; no process, connection or device is behind the panes, \
                   nothing is installed, and both answers are proved by the GPUI tests",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().chinese().smallest(),
        ],
        start: || page(Destination::Settings),
        prepare: seed_hosts,
        install: show_update_confirm,
        ready: update_confirm_ready,
    },
    Scenario {
        id: "overlay-palette",
        title: "浮层 · 命令面板",
        category: Category::Overlay,
        fixture: "six saved hosts; the palette opened over the hosts page",
        evidence: "shows the palette over a populated list; selecting an entry is not part of \
                   the shot",
        quick: false,
        variants: &[],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_palette,
        ready: palette_ready,
    },
    Scenario {
        id: "overlay-snippets",
        title: "浮层 · 快捷命令",
        category: Category::Overlay,
        fixture: "three saved commands, one of them a block, and one display-only session \
                  under the manager",
        evidence: "shows the manager, its fill-in and run actions and the line count of a \
                   block; pressing either action is not part of the shot",
        quick: false,
        variants: &[],
        start: || page(Destination::LocalShells),
        prepare: seed_snippets,
        install: show_snippets,
        ready: snippets_ready,
    },
    Scenario {
        id: "overlay-connect",
        title: "浮层 · 连接中的目标与阶段",
        category: Category::Overlay,
        fixture: "a saved host whose connection dialog reports the connecting stage; nothing \
                  was dialled",
        evidence: "shows the stage, the target, the wait and the two ways out the dialog \
                   draws; no transport, DNS lookup or authentication is part of it",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_connection,
        ready: connection_ready,
    },
    Scenario {
        id: "overlay-paste",
        title: "浮层 · 多行粘贴确认",
        category: Category::Overlay,
        fixture: "one display-only session with a five-line block held by the paste dialog",
        evidence: "shows the modal confirmation, its target, its line count and the scrolled \
                   preview; nothing was pasted into a process, because there is none",
        quick: false,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_paste,
        ready: paste_ready,
    },
    Scenario {
        id: "overlay-tabs",
        title: "浮层 · 全部标签",
        category: Category::Overlay,
        fixture: "three tabs, one of them carrying a background bell, listed over the terminal",
        evidence: "shows every tab with its target, its state and the shortcut that reaches \
                   it; no shell ran and the bell is a fixture mark",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_tab_list,
        ready: tab_list_ready,
    },
    Scenario {
        id: "batch-preview",
        title: "批量任务 · 确认前的目标与命令",
        category: Category::Overlay,
        fixture: "six saved hosts with four selected, two ordered commands, concurrency 4 and \
                  the skip-host policy",
        evidence: "shows the preview this build draws before anything is confirmed: the count of \
                   targets, their addresses, the commands and the limits. No session was opened and \
                   no command was sent, which is what the shot cannot show but the model proves",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_batch_preview,
        ready: batch_preview_ready,
    },
    Scenario {
        id: "batch-running",
        title: "批量任务 · 运行中的逐机逐步状态",
        category: Category::Overlay,
        fixture: "the real batch runner over four hosts and two commands, two hosts finished and \
                  two held inside the second command by the fixture gate",
        evidence: "shows the states, exit results, output and timeline the runner reported; the \
                   transport is a fixture, so this is not evidence about any SSH server",
        quick: true,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_batch_running,
        ready: batch_running_ready,
    },
    Scenario {
        id: "batch-failed",
        title: "批量任务 · 失败的主机与重试",
        category: Category::Overlay,
        fixture: "the same runner over four hosts, where one command exits 3 on one host and \
                  the other hosts succeed",
        evidence: "shows the failed host, its exit status and reason, the untouched hosts and the \
                   retry action; no rollback is claimed, because none happens",
        quick: true,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_batch_failed,
        ready: batch_failed_ready,
    },
    Scenario {
        id: "batch-paused",
        title: "批量任务 · 暂停只停止调度",
        category: Category::Overlay,
        fixture: "the runner holding every host inside its command, then paused",
        evidence: "shows that a pause starts no new step while the commands already running stay \
                   connected; nothing was stopped by it",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_batch_paused,
        ready: batch_paused_ready,
    },
    Scenario {
        id: "batch-library",
        title: "批量任务 · 任务库里的可复用定义",
        category: Category::Overlay,
        fixture: "three saved definitions on disk, one of them without a name and one with \
                  three steps; nothing on screen was written by this shot",
        evidence: "shows the library this build draws: the switch to the run and the history, the \
                   targets and steps of each definition, and the edit and delete actions. Opening \
                   the library reads the file and starts nothing",
        quick: true,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::Hosts),
        prepare: seed_batch_library,
        install: show_batch_library,
        ready: batch_library_ready,
    },
    Scenario {
        id: "batch-history",
        title: "批量任务 · 历史运行的逐机结果与时间线",
        category: Category::Overlay,
        fixture: "two stored runs written as the runner would have written them: one completed and \
                  one finished with a failure, with its log lines",
        evidence: "shows the detail of one stored run: the state of every host, the exit status of \
                   every step, the output rebuilt from the log and the offsets taken from it. The \
                   records are fixtures, so this is not evidence about any SSH server",
        quick: true,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::Hosts),
        prepare: seed_batch_history,
        install: show_batch_history,
        ready: batch_history_ready,
    },
    Scenario {
        id: "batch-interrupted",
        title: "批量任务 · 重启后中断的运行",
        category: Category::Overlay,
        fixture: "a run the last session left unfinished: started, one host done, the rest waiting",
        evidence: "shows the history after a start: the run is marked interrupted, what it had \
                   finished is still readable, and nothing was replayed to find that out",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_batch_interrupted,
        install: show_batch_history,
        ready: batch_interrupted_ready,
    },
    Scenario {
        id: "batch-background",
        title: "批量任务 · 转入后台后的顶栏指示",
        category: Category::Overlay,
        fixture: "the real batch runner over four hosts, left in the background while the host \
                  page is on screen",
        evidence: "shows the run still going with the page it was left from: the top bar carries \
                   the run and its progress, and choosing it returns to the same page. The \
                   transport is a fixture, so this is not evidence about any SSH server",
        quick: true,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_batch_background,
        ready: batch_background_ready,
    },
    Scenario {
        id: "batch-cancelling",
        title: "批量任务 · 取消中等待远端退出",
        category: Category::Overlay,
        fixture: "the runner with a cancel requested while every command is still running",
        evidence: "shows cancelling rather than cancelled: the page says the request was made and \
                   that the remote command has not ended, which is the state the run model waits in",
        quick: false,
        variants: &[Variant::dark().chinese()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_batch_cancelling,
        ready: batch_cancelling_ready,
    },
    Scenario {
        id: "menu-tab",
        title: "菜单 · 标签右键菜单",
        category: Category::Overlay,
        fixture: "a saved host's display-only terminal ended the way a lost connection ends, \
                  with its tab's right-click menu opened by the build",
        evidence: "shows the shared menu card: the reconnect entry first, the commands with \
                   their icons and shortcuts, the colour row and the disabled moves; no entry \
                   was pressed and no connection was made",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_tab_menu,
        ready: tab_menu_ready,
    },
    Scenario {
        id: "menu-host",
        title: "菜单 · 主机右键菜单",
        category: Category::Overlay,
        fixture: "six saved hosts, with the first one's right-click menu opened by the build",
        evidence: "shows the shared menu card over the hosts page and its danger entry; \
                   nothing was connected, copied or deleted",
        quick: false,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: show_host_menu,
        ready: host_menu_ready,
    },
    Scenario {
        id: "menu-files",
        title: "菜单 · 文件右键菜单",
        category: Category::Overlay,
        fixture: "both file columns answered from fixture listings, with the menu of one \
                  server file opened by the build",
        evidence: "shows the shared menu card over the file view and its danger entry; \
                   nothing was transferred, renamed or deleted",
        quick: false,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_files_menu,
        ready: files_menu_ready,
    },
    Scenario {
        id: "menu-terminal",
        title: "菜单 · 终端右键菜单",
        category: Category::Overlay,
        fixture: "the local terminal fed with the transcript fixture, with its right-click \
                  menu opened by the build",
        evidence: "shows the shared menu card over the grid with the shortcuts in force and \
                   copy unavailable while nothing is selected; nothing was copied or pasted",
        quick: false,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest().largest(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_terminal_menu,
        ready: terminal_menu_ready,
    },
    Scenario {
        id: "menu-shell",
        title: "菜单 · 新建标签下拉菜单",
        category: Category::Overlay,
        fixture: "the local terminal with the header's shell menu opened by the build; the \
                  shells listed are the ones this machine reports",
        evidence: "shows the shared menu card hanging from its control, its headings and the \
                   current shell's check; no shell was started",
        quick: false,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_shell_menu,
        ready: shell_menu_ready,
    },
    Scenario {
        id: "startup-splash",
        title: "启动 · 品牌过场",
        category: Category::Startup,
        fixture: "an empty data directory; the window built and then held on its start,                   the brand drawn in its final place",
        evidence: "shows the frame a launch draws while the stores are read: the live caption                    and the logo, name and tagline on the floor; the entrance, the fade into the                    page and how long a real read takes are motion and timing a still frame                    cannot show",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().smallest(),
            Variant::light().smallest().largest(),
        ],
        start: || page(Destination::Hosts),
        prepare: nothing,
        install: hold_splash,
        ready: splash_ready,
    },
    Scenario {
        id: "design-catalog",
        title: "设计 · 组件清单",
        category: Category::Design,
        fixture: "no data; the window draws the component catalogue (src/app/catalog.rs) \
                  instead of a page, with fixed English sample words",
        evidence: "shows every shared component in its states on the token v2 palette: \
                   buttons, fields, search, choices, labels, banners, progress, skeleton, \
                   sidebar, list rows, menu, toast and status card; hover is drawn from \
                   the hovered face rather than a pointer, the indeterminate bar is the \
                   still form a capture's reduced motion draws, and nothing on the page \
                   is wired to an action",
        quick: false,
        variants: &[],
        start: || page(Destination::Hosts),
        prepare: nothing,
        install: show_catalog,
        ready: catalog_ready,
    },
];

/// Applies a shot's palette, language, text size and motion, then the scenario's own
/// state, in that order: a scenario that opens a tool has to open it with the language
/// the shot is drawn in.
pub fn install(
    scenario: &Scenario,
    variant: Variant,
    view: &mut ShelluneView,
    window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.set_capture_theme(variant.theme, cx);
    view.set_capture_locale(variant.locale, cx);
    view.set_capture_zoom(variant.zoom, cx);
    view.set_capture_motion(Motion::Reduced, cx);
    (scenario.install)(view, window, cx)
}

// ---------------------------------------------------------------------------
// Batch tasks (R19, R21)
// ---------------------------------------------------------------------------

/// The gate of the run a batch scenario started, kept alive for the shot.
///
/// It lives beside the scenarios because the capture takes its frame after install has
/// returned: a gate dropped at the end of install would let every held command end before
/// the shot, and the page would show a finished run where a running one belongs.
static BATCH_GATE: std::sync::Mutex<Option<BatchGate>> = std::sync::Mutex::new(None);

type BatchGate = crate::app::view::batch::fixture::Gate;

/// The four hosts a batch shot runs on: the first four the fixture writes.
const BATCH_TARGETS: [&str; 4] = ["build-01", "api-02", "edge-03", "stage-01"];

/// Selects those hosts and opens the preview, with the commands a person would type.
fn open_batch(
    view: &mut ShelluneView,
    commands: &[&str],
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_batch_select(BATCH_TARGETS.len(), cx);
    view.capture_batch_preview(cx);
    view.capture_batch_commands(commands, cx)
}

/// Starts a run whose commands the fixture scripts, and keeps its gate for the shot.
fn start_batch(
    view: &mut ShelluneView,
    plan: crate::app::view::batch::fixture::Plan,
    cx: &mut Context<ShelluneView>,
) {
    let (_, gate) = view.capture_batch_start(plan, cx);
    let mut slot = match BATCH_GATE.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = Some(gate);
}

/// The gate of the run on screen, when the caller has to let a command end.
fn batch_gate() -> Option<BatchGate> {
    let mut slot = match BATCH_GATE.lock() {
        Ok(slot) => slot,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.take()
}

fn show_batch_preview(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    open_batch(view, &["uptime", "df -h /"], cx)
}

fn batch_preview_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Populated)?;
    if view.capture_overlay() != Some("batch-preview") {
        return Err(format!(
            "the batch preview is not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let panel = view.batch_panel();
    if panel.target_count() != BATCH_TARGETS.len() {
        return Err(format!(
            "the preview covers {} host(s), not the {} the shot names",
            panel.target_count(),
            BATCH_TARGETS.len()
        ));
    }
    if panel.steps().len() != 2 {
        return Err(format!(
            "the preview holds {} step(s), not the two commands the shot writes",
            panel.steps().len()
        ));
    }
    Ok(())
}

fn show_batch_running(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    use crate::app::view::batch::fixture::{HostScript, Plan, StepScript};
    open_batch(view, &["uptime", "journalctl -u shellune -n 40"], cx)?;
    let finished = HostScript::new(vec![
        StepScript::ok(" 09:20:11 up 12 days,  load average: 0.08, 0.11, 0.09\n"),
        StepScript::ok("-- Journal begins --\nNov 12 09:14:03 shellune[418]: listening\n"),
    ]);
    let held = HostScript::new(vec![
        StepScript::ok(" 09:20:14 up 12 days,  load average: 0.12, 0.10, 0.09\n"),
        StepScript::holding("-- Journal begins --\nNov 12 09:15:41 shellune[421]: syncing\n"),
    ]);
    start_batch(
        view,
        Plan::new()
            .host(BATCH_TARGETS[0], finished.clone())
            .host(BATCH_TARGETS[1], finished)
            .host(BATCH_TARGETS[2], held.clone())
            .host(BATCH_TARGETS[3], held),
        cx,
    );
    let ready = view.capture_batch_wait(cx, |view| {
        view.batch_panel().run().is_some_and(|page| {
            page.run()
                .hosts()
                .iter()
                .filter(|host| host.state() == shellune_core::HostState::Succeeded)
                .count()
                >= 2
                && page.running_hosts() >= 2
        })
    });
    if !ready {
        return Err("the run never reached two finished hosts and two running ones".to_owned());
    }
    Ok(())
}

fn batch_running_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay() != Some("batch-run") {
        return Err(format!(
            "the batch run page is not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let panel = view.batch_panel();
    let page = panel
        .run()
        .ok_or_else(|| "the batch run page is not on screen".to_owned())?;
    if page.state() != shellune_core::RunState::Running {
        return Err(format!("the run is {:?}, not running", page.state()));
    }
    if page.finished_hosts() < 2 || page.running_hosts() < 2 {
        return Err(format!(
            "the run reports {} finished and {} running host(s)",
            page.finished_hosts(),
            page.running_hosts()
        ));
    }
    Ok(())
}

fn show_batch_failed(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    use crate::app::view::batch::fixture::{HostScript, Plan, StepScript};
    open_batch(view, &["uptime", "deploy --check"], cx)?;
    let good = HostScript::new(vec![
        StepScript::ok(" 09:20:11 up 12 days\n"),
        StepScript::ok("release 0.4.0 is deployed\n"),
    ]);
    let failing = HostScript::new(vec![
        StepScript::ok(" 09:20:14 up 12 days\n"),
        StepScript::failing(3, "deploy: no such release: 0.4.1\n"),
    ]);
    start_batch(
        view,
        Plan::new()
            .host(BATCH_TARGETS[0], good.clone())
            .host(BATCH_TARGETS[1], failing)
            .host(BATCH_TARGETS[2], good.clone())
            .host(BATCH_TARGETS[3], good),
        cx,
    );
    let ready = view.capture_batch_wait(cx, |view| {
        view.batch_panel()
            .run()
            .is_some_and(crate::app::batch::BatchRunPage::is_finished)
    });
    if !ready {
        return Err("the run never finished".to_owned());
    }
    // This run is over, so its gate has nothing left to hold.
    let _ = batch_gate();
    Ok(())
}

fn batch_failed_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay() != Some("batch-run") {
        return Err(format!(
            "the batch run page is not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let panel = view.batch_panel();
    let page = panel
        .run()
        .ok_or_else(|| "the batch run page is not on screen".to_owned())?;
    if !page.is_finished() {
        return Err("the run page still reports the run as going".to_owned());
    }
    if page.state() != shellune_core::RunState::Failed {
        return Err(format!("the run ended {:?}, not failed", page.state()));
    }
    if !page.can_retry() {
        return Err("the page offers no retry for a run with a failure".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The task centre (R11, R12, R13)
// ---------------------------------------------------------------------------

/// Builds one batch definition over the named saved hosts of the fixture.
fn batch_task(
    root: &Path,
    commands: &[&str],
    hosts: &[&str],
    name: Option<&str>,
) -> Result<shellune_core::BatchTask, String> {
    let book = HostStore::at(root)
        .load()
        .map_err(|error| format!("the host fixture could not be read: {error}"))?;
    let defaults = shellune_core::ConnectionDefaults::default();
    let mut targets = Vec::new();
    for wanted in hosts {
        let host = book
            .hosts()
            .iter()
            .find(|host| host.name() == *wanted)
            .ok_or_else(|| format!("the host fixture has no host {wanted}"))?;
        targets.push(
            shellune_core::BatchTarget::snapshot(host, defaults)
                .map_err(|error| format!("the target {wanted} is not valid: {error}"))?,
        );
    }
    let steps = shellune_core::BatchTask::steps_from_commands(
        commands.iter().map(|line| (*line).to_owned()),
    )
    .map_err(|error| format!("the commands are not valid steps: {error}"))?;
    let mut task = shellune_core::BatchTask::new(steps, targets, 2)
        .map_err(|error| format!("the task is not valid: {error}"))?;
    if let Some(name) = name {
        task.set_name(Some(name.to_owned()));
    }
    Ok(task)
}

/// Writes definitions as an earlier session would have left them.
fn seed_batch_library(root: &Path) -> Result<(), String> {
    seed_hosts(root)?;
    let store = shellune_storage::BatchStore::at(root);
    let mut book = store
        .load_tasks_upgrading()
        .map_err(|error| format!("the tasks file could not be read: {error}"))?;
    for (commands, hosts, name) in [
        (
            &["uptime", "df -h /"][..],
            &["build-01", "api-02", "edge-03", "stage-01"][..],
            Some("Nightly check"),
        ),
        (
            &["journalctl -u shellune -n 40 --no-pager"][..],
            &["stage-02", "lab-07"][..],
            Some("Tailing the journal"),
        ),
        (
            &["git fetch --all", "git status --short", "make check"][..],
            &["build-01"][..],
            None,
        ),
    ] {
        book.insert(batch_task(root, commands, hosts, name)?);
    }
    store
        .save_tasks(&book)
        .map_err(|error| format!("the tasks fixture could not be written: {error}"))
}

/// Appends one event to a run's log and folds it into the same run.
fn append_run_event(
    store: &shellune_storage::BatchStore,
    run: &mut shellune_core::BatchRun,
    seq: &mut u64,
    kind: shellune_core::BatchEventKind,
    at: u64,
) -> Result<(), String> {
    *seq += 1;
    store
        .append(run.id(), kind.clone(), at)
        .map_err(|error| format!("the run log could not be written: {error}"))?;
    run.apply(&shellune_core::BatchEvent::new(*seq, at, kind))
        .map_err(|error| format!("the run refused its own history: {error}"))
}

/// Writes stored runs: one that completed, one that ended with a failure and its log.
fn seed_batch_history(root: &Path) -> Result<(), String> {
    seed_hosts(root)?;
    let store = shellune_storage::BatchStore::at(root);
    for (commands, hosts, failed) in [
        (
            &["uptime", "df -h /"][..],
            &["build-01", "api-02"][..],
            false,
        ),
        (
            &["uptime", "deploy --check"][..],
            &["stage-01", "stage-02", "lab-07"][..],
            true,
        ),
    ] {
        let task = batch_task(root, commands, hosts, Some("Nightly check"))?;
        let confirmed = task
            .confirm(1_700_000_000)
            .map_err(|error| format!("the run fixture is not confirmable: {error}"))?;
        let mut run = shellune_core::BatchRun::planned(
            &confirmed,
            shellune_core::BatchRunId::new(),
            1_700_000_000,
        );
        let mut seq = 0u64;
        let host_ids: Vec<shellune_core::HostId> = run
            .hosts()
            .iter()
            .map(|host| host.target().host_id())
            .collect();
        append_run_event(
            &store,
            &mut run,
            &mut seq,
            shellune_core::BatchEventKind::RunStarted {
                hosts: host_ids.len(),
                steps: commands.len(),
                concurrency: 2,
            },
            1_700_000_000,
        )?;
        for (index, id) in host_ids.iter().copied().enumerate() {
            let at = 1_700_000_010 + index as u64 * 4;
            append_run_event(
                &store,
                &mut run,
                &mut seq,
                shellune_core::BatchEventKind::HostState {
                    host: id,
                    state: shellune_core::HostState::Running,
                },
                at,
            )?;
            for (step, command) in commands.iter().enumerate() {
                let step_at = at + step as u64;
                append_run_event(
                    &store,
                    &mut run,
                    &mut seq,
                    shellune_core::BatchEventKind::StepStarted {
                        host: id,
                        step: step as u32,
                    },
                    step_at,
                )?;
                append_run_event(
                    &store,
                    &mut run,
                    &mut seq,
                    shellune_core::BatchEventKind::Output {
                        host: id,
                        step: step as u32,
                        stream: shellune_core::OutputStream::Stdout,
                        chunk: match (*command, failed && index == 1 && step == 1) {
                            ("uptime", _) => {
                                " 09:20:11 up 12 days,  load average: 0.08\n".to_owned()
                            }
                            ("df -h /", _) => {
                                "/dev/nvme0n1p2  476G   98G  354G  22% /\n".to_owned()
                            }
                            (_, true) => "deploy: no such release: 0.4.1\n".to_owned(),
                            _ => "release 0.4.0 is deployed\n".to_owned(),
                        },
                    },
                    step_at + 1,
                )?;
                let failure = failed && index == 1 && step == 1;
                append_run_event(
                    &store,
                    &mut run,
                    &mut seq,
                    shellune_core::BatchEventKind::StepFinished {
                        host: id,
                        step: step as u32,
                        state: if failure {
                            shellune_core::StepState::Failed
                        } else {
                            shellune_core::StepState::Succeeded
                        },
                        exit: Some(if failure { 3 } else { 0 }),
                        detail: failure.then(|| "deploy: no such release: 0.4.1".to_owned()),
                    },
                    step_at + 2,
                )?;
            }
            let host_failed = failed && index == 1;
            append_run_event(
                &store,
                &mut run,
                &mut seq,
                shellune_core::BatchEventKind::HostFinished {
                    host: id,
                    state: if host_failed {
                        shellune_core::HostState::Failed
                    } else {
                        shellune_core::HostState::Succeeded
                    },
                    detail: host_failed.then(|| "step 2 exited 3".to_owned()),
                },
                at + 5,
            )?;
        }
        append_run_event(
            &store,
            &mut run,
            &mut seq,
            shellune_core::BatchEventKind::RunState {
                state: if failed {
                    shellune_core::RunState::Failed
                } else {
                    shellune_core::RunState::Completed
                },
            },
            1_700_000_100,
        )?;
        store
            .save_run(&run)
            .map_err(|error| format!("the run summary could not be written: {error}"))?;
    }
    Ok(())
}

/// Writes one run the last session left unfinished.
fn seed_batch_interrupted(root: &Path) -> Result<(), String> {
    seed_hosts(root)?;
    let store = shellune_storage::BatchStore::at(root);
    let task = batch_task(
        root,
        &["uptime", "df -h /"],
        &["build-01", "api-02", "edge-03"],
        Some("Nightly check"),
    )?;
    let confirmed = task
        .confirm(1_700_000_000)
        .map_err(|error| format!("the run fixture is not confirmable: {error}"))?;
    let mut run = shellune_core::BatchRun::planned(
        &confirmed,
        shellune_core::BatchRunId::new(),
        1_700_000_000,
    );
    let first = run.hosts()[0].target().host_id();
    let mut seq = 0u64;
    for (kind, at) in [
        (
            shellune_core::BatchEventKind::RunStarted {
                hosts: 3,
                steps: 2,
                concurrency: 2,
            },
            1_700_000_000,
        ),
        (
            shellune_core::BatchEventKind::HostState {
                host: first,
                state: shellune_core::HostState::Running,
            },
            1_700_000_004,
        ),
        (
            shellune_core::BatchEventKind::StepStarted {
                host: first,
                step: 0,
            },
            1_700_000_005,
        ),
        (
            shellune_core::BatchEventKind::Output {
                host: first,
                step: 0,
                stream: shellune_core::OutputStream::Stdout,
                chunk: " 09:20:11 up 12 days,  load average: 0.08\n".to_owned(),
            },
            1_700_000_006,
        ),
        (
            shellune_core::BatchEventKind::StepFinished {
                host: first,
                step: 0,
                state: shellune_core::StepState::Succeeded,
                exit: Some(0),
                detail: None,
            },
            1_700_000_007,
        ),
        (
            shellune_core::BatchEventKind::HostFinished {
                host: first,
                state: shellune_core::HostState::Failed,
                detail: Some("the connection was closed".to_owned()),
            },
            1_700_000_008,
        ),
    ] {
        append_run_event(&store, &mut run, &mut seq, kind, at)?;
    }
    store
        .save_run(&run)
        .map_err(|error| format!("the run summary could not be written: {error}"))
}

fn show_batch_library(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_batch_library(cx);
    Ok(())
}

fn batch_library_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Populated)?;
    if view.capture_overlay() != Some("batch-library") {
        return Err(format!(
            "the saved tasks are not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let count = view.batch_panel().library().tasks().len();
    if count != 3 {
        return Err(format!(
            "the library lists {count} task(s), not the three saved"
        ));
    }
    Ok(())
}

fn show_batch_history(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    view.capture_batch_history(cx);
    // The shot is about the run that carries a failure, which is the one with the exit
    // status, the reason and the output worth reading. The two fixtures are written in
    // the same second, so the order they are listed in is the store's, not the shot's:
    // every entry is tried and the first failed one is the one that is opened.
    let mut opened = false;
    for index in 0..8 {
        if !view.capture_batch_stored(index, cx) {
            break;
        }
        opened = true;
        let failed = view
            .batch_panel()
            .stored_summary()
            .is_some_and(|run| run.state() == shellune_core::RunState::Failed);
        if failed {
            break;
        }
    }
    if !opened {
        return Err("no stored run could be opened".to_owned());
    }
    Ok(())
}

fn batch_history_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    on_page(view, Destination::Hosts, SectionState::Populated)?;
    if view.capture_overlay() != Some("batch-history") {
        return Err(format!(
            "the history is not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let panel = view.batch_panel();
    let run = panel
        .stored_summary()
        .ok_or_else(|| "no stored run is open".to_owned())?;
    if run.state() != shellune_core::RunState::Failed {
        return Err(format!("the open run ended {:?}, not failed", run.state()));
    }
    if run.hosts().len() != 3 {
        return Err(format!(
            "the open run has {} host(s), not the three the fixture wrote",
            run.hosts().len()
        ));
    }
    if panel.stored_detail_open().is_none() {
        return Err("the output of the chosen host was not read from the log".to_owned());
    }
    Ok(())
}

fn batch_interrupted_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay() != Some("batch-history") {
        return Err(format!(
            "the history is not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let panel = view.batch_panel();
    let state = panel
        .stored_summary()
        .map(shellune_core::BatchRun::state)
        .ok_or_else(|| "no stored run is open".to_owned())?;
    if state != shellune_core::RunState::Interrupted {
        return Err(format!("the stored run is {state:?}, not interrupted"));
    }
    if panel.has_active_run() {
        return Err("a restart must not start a runner".to_owned());
    }
    Ok(())
}

fn show_batch_background(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    use crate::app::view::batch::fixture::{HostScript, Plan, StepScript};
    open_batch(view, &["journalctl -u shellune -f"], cx)?;
    let held = HostScript::new(vec![StepScript::holding(
        "Nov 12 09:15:41 shellune[421]: tailing the journal\n",
    )]);
    start_batch(view, Plan::new().every(&BATCH_TARGETS, held), cx);
    let ready = view.capture_batch_wait(cx, |view| {
        view.batch_panel()
            .run()
            .is_some_and(|page| page.running_hosts() == BATCH_TARGETS.len())
    });
    if !ready {
        return Err("the run never had every host inside its command".to_owned());
    }
    view.capture_batch_background(cx);
    Ok(())
}

fn batch_background_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay().is_some() {
        return Err(format!(
            "a background run must leave the page free: {:?}",
            view.capture_overlay()
        ));
    }
    let panel = view.batch_panel();
    if !panel.is_watching() {
        return Err("the run stopped when the page was left".to_owned());
    }
    let page = panel
        .run()
        .ok_or_else(|| "the run page was dropped by leaving".to_owned())?;
    if page.running_hosts() != BATCH_TARGETS.len() {
        return Err(format!(
            "{} host(s) are still running, not the four the shot names",
            page.running_hosts()
        ));
    }
    Ok(())
}

/// Starts a run every host is held inside, and leaves it there.
fn hold_every_host(view: &mut ShelluneView, cx: &mut Context<ShelluneView>) -> Result<(), String> {
    use crate::app::view::batch::fixture::{HostScript, Plan, StepScript};
    open_batch(view, &["journalctl -u shellune -f"], cx)?;
    let held = HostScript::new(vec![StepScript::holding(
        "Nov 12 09:15:41 shellune[421]: tailing the journal\n",
    )]);
    start_batch(view, Plan::new().every(&BATCH_TARGETS, held), cx);
    let ready = view.capture_batch_wait(cx, |view| {
        view.batch_panel()
            .run()
            .is_some_and(|page| page.running_hosts() == BATCH_TARGETS.len())
    });
    if !ready {
        return Err("the run never had every host inside its command".to_owned());
    }
    Ok(())
}

fn show_batch_paused(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    hold_every_host(view, cx)?;
    view.capture_batch_pause(cx);
    let ready = view.capture_batch_wait(cx, |view| {
        view.batch_panel()
            .run()
            .is_some_and(|page| page.state() == shellune_core::RunState::Paused)
    });
    if !ready {
        return Err("the run never reported the pause".to_owned());
    }
    Ok(())
}

fn batch_paused_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay() != Some("batch-run") {
        return Err(format!(
            "the batch run page is not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let page = view
        .batch_panel()
        .run()
        .ok_or_else(|| "the batch run page is not on screen".to_owned())?;
    if page.state() != shellune_core::RunState::Paused {
        return Err(format!("the run is {:?}, not paused", page.state()));
    }
    if page.running_hosts() != BATCH_TARGETS.len() {
        return Err("a pause must leave the commands that are running where they are".to_owned());
    }
    Ok(())
}

fn show_batch_cancelling(
    view: &mut ShelluneView,
    _window: &mut Window,
    cx: &mut Context<ShelluneView>,
) -> Result<(), String> {
    hold_every_host(view, cx)?;
    view.capture_batch_cancel(cx);
    let ready = view.capture_batch_wait(cx, |view| {
        view.batch_panel()
            .run()
            .is_some_and(|page| page.state() == shellune_core::RunState::Cancelling)
    });
    if !ready {
        return Err("the run never reported the cancel request".to_owned());
    }
    Ok(())
}

fn batch_cancelling_ready(view: &ShelluneView, _cx: &App) -> Result<(), String> {
    if view.capture_overlay() != Some("batch-run") {
        return Err(format!(
            "the batch run page is not the surface on screen: {:?}",
            view.capture_overlay()
        ));
    }
    let page = view
        .batch_panel()
        .run()
        .ok_or_else(|| "the batch run page is not on screen".to_owned())?;
    if page.state() != shellune_core::RunState::Cancelling {
        return Err(format!("the run is {:?}, not cancelling", page.state()));
    }
    if page.is_finished() {
        return Err("a cancelling run is not a finished one".to_owned());
    }
    Ok(())
}
