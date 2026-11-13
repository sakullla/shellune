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
use crate::app::keymap::Problem;
use crate::app::nav::{Destination, Platform};
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
    /// The settings page.
    Settings,
    /// A tool or palette open over a page.
    Overlay,
}

impl Category {
    /// Stable identifier written to the manifest.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Terminal => "terminal",
            Self::Settings => "settings",
            Self::Overlay => "overlay",
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
    let expected = HOST_IDS.len();
    let listed = view.book().hosts().len();
    if listed != expected {
        return Err(format!(
            "the hosts page lists {listed} host(s), not the {expected} the fixture holds"
        ));
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
        evidence: "shows the empty home page; says nothing about a host list with items",
        quick: false,
        variants: &[Variant::dark().smallest()],
        start: || page(Destination::Hosts),
        prepare: nothing,
        install: no_state,
        ready: home_empty_ready,
    },
    Scenario {
        id: "home-hosts",
        title: "首页 · 已保存的主机与分组",
        category: Category::Home,
        fixture: "six saved hosts in three groups, seeded on disk before the view starts",
        evidence: "shows the list this build draws; the hosts were never connected and no \
                   group tree exists yet",
        quick: true,
        variants: &[
            Variant::dark().chinese(),
            Variant::dark().comfortable(),
            Variant::dark().smallest(),
        ],
        start: || page(Destination::Hosts),
        prepare: seed_hosts,
        install: no_state,
        ready: home_hosts_ready,
    },
    Scenario {
        id: "terminal-local",
        title: "本地终端 · 会话与标签",
        category: Category::Terminal,
        fixture: "one display-only session fed a fixed transcript; no PTY or shell ran",
        evidence: "proves terminal layout, colour and text rendering; proves nothing about a \
                   live shell, its environment or its keystrokes",
        quick: true,
        variants: &[
            Variant::dark().smallest(),
            Variant::dark().comfortable(),
            Variant::dark().chinese(),
        ],
        start: || page(Destination::LocalShells),
        prepare: nothing,
        install: show_terminal,
        ready: terminal_ready,
    },
    Scenario {
        id: "terminal-tabs",
        title: "终端 · 多标签与后台提醒",
        category: Category::Terminal,
        fixture: "two display-only shells and one saved host's terminal, the second tab \
                  carrying a background bell",
        evidence: "proves the merged head names the current tab once, and that another tab's \
                   bell is marked while it is not the one on screen; no shell, SSH session or \
                   bell from a real program is part of it",
        quick: true,
        variants: &[Variant::dark().chinese(), Variant::dark().smallest()],
        start: || page(Destination::LocalShells),
        prepare: seed_hosts,
        install: show_tabs,
        ready: tabs_ready,
    },
    Scenario {
        id: "terminal-split",
        title: "终端 · 左右分屏",
        category: Category::Terminal,
        fixture: "one session split left and right, the second pane holding a second \
                  display-only session",
        evidence: "proves a split marks its panes and its active pane without repeating the \
                   session; the second pane has no process behind it",
        quick: true,
        variants: &[
            Variant::dark().comfortable().chinese(),
            Variant::dark().smallest(),
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
        evidence: "proves the state, the reason and the next step are readable on the merged \
                   head; no connection was ever made, so no transport behaviour is shown",
        quick: true,
        variants: &[Variant::dark().chinese()],
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
        ],
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
        title: "设置 · 连接分类只读策略",
        category: Category::Settings,
        fixture: "default preferences; the connection policy the SSH layer applies",
        evidence: "shows the second level of the navigation on the connection category and the \
                   values in force; nothing was dialled and no policy was changed",
        quick: false,
        variants: &[Variant::dark().chinese().smallest()],
        start: || page(Destination::Settings),
        prepare: nothing,
        install: |view, window, cx| show_settings_item(view, window, cx, Item::Policy),
        ready: settings_connection_ready,
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
