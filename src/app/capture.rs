//! The hidden-window capture tool (R1, R2, R9, R11).
//!
//! This is the second capture entry. The first one proved the question this whole
//! workflow depends on - can the real Shellune view be drawn in a window the user never
//! sees and read back as real pixels? - and it is answered: yes, through the hidden
//! window's own GPU back buffer. This entry turns that probe into a tool:
//!
//! * [`scenarios`] registers the states worth photographing, in the real view, each with
//!   what it was built from and what it cannot prove;
//! * a run is [`--theme`], [`--size`], [`--only`], [`--quick`] or a full matrix, and
//!   each shot names its palette, logical and physical size, DPI, language and text size
//!   in the manifest it writes beside the PNGs;
//! * a shot is taken only after the scenario says it arrived and after the window drew
//!   two more frames, and the frame is only accepted when two consecutive readbacks are
//!   byte-identical, so a fixed sleep is never the reason a shot is right;
//! * a shot is drawn from its own state and from nothing the run did before it: the
//!   isolated stores are named after the shot rather than after its place in the plan,
//!   because the shell prints where its records live and that path is pixels. The same
//!   shot taken in a quick, an `--only` or a full run is therefore the same picture;
//! * every store lives in a temporary directory that is removed again, credentials are in
//!   memory, and no PTY, SSH connection, ProxyCommand or user command is started;
//! * a failure - a missing scenario state, a blank or wrong-sized frame, a write error -
//!   ends the run non-zero and leaves what it managed to write for the reader. The
//!   gallery and the previous round's images are published by [`scripts/capture-ui.py`],
//!   which is also what keeps a failed run from replacing the baseline.
//!
//! What this tool cannot do is part of the evidence, and it is written into the manifest:
//! the pixels come from one Windows DirectX readback, the sessions are display-only
//! fixtures, and a PNG of a page is not proof that the page works.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

pub mod scenarios;

use scenarios::{SCENARIOS, Scenario, Variant};

// Everything below the planner is the Windows half: a platform without the hidden-window
// readback still parses the same arguments, still lists the scenarios, and answers with
// the one report that cannot be mistaken for a rendering result.
#[cfg(target_os = "windows")]
use std::sync::{Arc, Mutex};
#[cfg(target_os = "windows")]
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use gpui::{
    App, AppContext as _, Application, AsyncWindowContext, Bounds, TitlebarOptions, Window,
    WindowBounds, WindowOptions, point, px, size,
};

#[cfg(target_os = "windows")]
use super::ShelluneView;
#[cfg(target_os = "windows")]
use super::icons;
#[cfg(target_os = "windows")]
use super::nav::Platform;
#[cfg(target_os = "windows")]
use super::product;
#[cfg(target_os = "windows")]
use super::view;
#[cfg(target_os = "windows")]
use super::workspace::Workspace;

#[cfg(target_os = "windows")]
mod windows;

/// The flag that sends the binary to the capture tool instead of the application.
const FLAG: &str = "--ui-capture";

/// The manifest the gallery and the next run read.
const MANIFEST_FILE: &str = "capture-ui-manifest.json";

const USAGE: &str = "\
usage: shellune --ui-capture [--list] [--quick] [--out <directory>]
                             [--theme dark|light|both] [--size <width>[x<height>]]
                             [--only <pattern>[,<pattern>...]]

  --list                print the scenarios and their shots as JSON, capture nothing
  --quick               only the main pages, in the dark palette
  --out <directory>     where the PNGs and their manifest go; the gallery script
                        passes its own staging directory
  --theme <choice>      dark, light or both (default: both; quick defaults to dark)
  --size <width>[x<h>]  keep the shots at this logical width (and height, if given);
                        repeat the flag to keep more than one
  --only <pattern>      keep the scenarios whose id matches (glob: * and ?); repeat or
                        separate with commas

A run draws each scenario in the real shell inside a window that is never shown, waits
for the state and for two identical GPU frames, reads the frame back and writes it beside
a manifest. It never shows a window, never reads the screen or the desktop, and never
starts a shell, a connection or a user command. A shot that cannot be taken ends the run
non-zero.";

// The waits and the frame floors below belong to the Windows half: on a platform
// without the hidden-window readback nothing waits for a frame or judges one, and a
// `Duration` there would be an unused import that stops the build outright - a platform
// that cannot compile its tests cannot run them either.
/// Frames a shot lets the shell draw after its state is installed, and how long a step
/// may wait for the platform to deliver them at all.
#[cfg(target_os = "windows")]
const SETTLE_FRAMES: u32 = 3;
#[cfg(target_os = "windows")]
const SETTLE_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(target_os = "windows")]
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// How long two identical readbacks must be apart before the shot is called settled.
///
/// Two frames a few milliseconds apart can both be drawn before an image asset has been
/// decoded, so "the same picture twice" alone would accept a frame that is still waiting
/// for its icons. The window is an observation, not the reason a shot is right: the
/// condition is still that the pixels stopped changing, and a frame that keeps changing
/// inside the window is never accepted.
#[cfg(target_os = "windows")]
const STABLE_WINDOW: Duration = Duration::from_millis(150);

/// How long a whole run may take before the watchdog ends it with a report.
#[cfg(target_os = "windows")]
const WATCHDOG_BASE: Duration = Duration::from_secs(120);
#[cfg(target_os = "windows")]
const WATCHDOG_PER_SHOT: Duration = Duration::from_secs(30);

/// A real page is not one colour; this is the floor below which the image is not a page.
#[cfg(target_os = "windows")]
const DISTINCT_COLOR_FLOOR: usize = 16;

/// Entry point. Returns the process exit status: 0 for a run that captured every shot,
/// 1 for one that did not, 2 for a bad invocation, 3 for an unsupported platform.
pub fn main(arguments: Vec<String>) -> i32 {
    let options = match Options::parse(&arguments) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("shellune ui-capture: {message}\n{USAGE}");
            return 2;
        }
    };
    if options.list {
        print_scenarios();
        return 0;
    }
    let shots = match plan(&options) {
        Ok(shots) => shots,
        Err(message) => {
            eprintln!("shellune ui-capture: {message}\n{USAGE}");
            return 2;
        }
    };
    #[cfg(target_os = "windows")]
    disable_direct_composition();
    run(options, shots)
}

/// Whether these arguments ask for the capture tool.
#[must_use]
pub fn invoked(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == FLAG)
}

/// Which palettes a run keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThemeFilter {
    Both,
    Dark,
    Light,
}

impl ThemeFilter {
    fn keeps(self, theme: super::theme::ThemeChoice) -> bool {
        match self {
            Self::Both => true,
            Self::Dark => theme == super::theme::ThemeChoice::Dark,
            Self::Light => theme == super::theme::ThemeChoice::Light,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// What one run was asked to do.
#[derive(Clone, Debug)]
struct Options {
    /// Whether the run is the quick one (main pages, dark palette).
    quick: bool,
    /// Whether it only lists the scenarios.
    list: bool,
    /// Which palettes to keep.
    theme: ThemeFilter,
    /// Logical sizes to keep: a width, and a height when the caller gave one.
    sizes: Vec<(f32, Option<f32>)>,
    /// Scenario id patterns. Empty means every scenario.
    only: Vec<String>,
    /// Where the PNGs and the manifest go.
    out: PathBuf,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            quick: false,
            list: false,
            theme: ThemeFilter::Both,
            sizes: Vec::new(),
            only: Vec::new(),
            out: default_out(),
        }
    }
}

/// The directory the tool writes to when nobody says otherwise.
///
/// The gallery script always passes its staging directory; this default exists so the
/// binary can be run by hand.
fn default_out() -> PathBuf {
    PathBuf::from("target").join("ui-capture").join("shots")
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut options = Self::default();
        let mut theme_given = false;
        let mut arguments = arguments
            .iter()
            .filter(|argument| argument.as_str() != FLAG)
            .peekable();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--out" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--out needs a directory".to_owned())?;
                    options.out = PathBuf::from(value);
                }
                "--quick" => options.quick = true,
                "--list" => options.list = true,
                "--theme" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--theme needs dark, light or both".to_owned())?;
                    options.theme = match value.as_str() {
                        "dark" => ThemeFilter::Dark,
                        "light" => ThemeFilter::Light,
                        "both" => ThemeFilter::Both,
                        other => {
                            return Err(format!(
                                "--theme wants dark, light or both, not {other:?}"
                            ));
                        }
                    };
                    theme_given = true;
                }
                "--size" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--size needs <width> or <width>x<height>".to_owned())?;
                    options.sizes.push(parse_size(value)?);
                }
                "--only" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--only needs a scenario pattern".to_owned())?;
                    let patterns: Vec<String> = value
                        .split(',')
                        .map(str::trim)
                        .filter(|pattern| !pattern.is_empty())
                        .map(str::to_owned)
                        .collect();
                    if patterns.is_empty() {
                        return Err("--only needs a scenario pattern".to_owned());
                    }
                    options.only.extend(patterns);
                }
                "--help" | "-h" => return Err("help requested".to_owned()),
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        // The quick run is the dark one unless the caller asked for a palette.
        if options.quick && !theme_given {
            options.theme = ThemeFilter::Dark;
        }
        Ok(options)
    }

    /// `quick` or `full`, as the manifest spells the run.
    fn mode(&self) -> &'static str {
        if self.quick { "quick" } else { "full" }
    }
}

/// Reads `1120` or `1120x720`.
fn parse_size(value: &str) -> Result<(f32, Option<f32>), String> {
    let (width, height) = match value.split_once(['x', 'X']) {
        Some((width, height)) => (width, Some(height)),
        None => (value, None),
    };
    let width =
        parse_length(width).ok_or_else(|| format!("--size width is not a number: {value:?}"))?;
    let height = match height {
        Some(height) => Some(
            parse_length(height)
                .ok_or_else(|| format!("--size height is not a number: {value:?}"))?,
        ),
        None => None,
    };
    Ok((width, height))
}

fn parse_length(value: &str) -> Option<f32> {
    let number = value.trim().parse::<f32>().ok()?;
    (number.is_finite() && number > 0.0).then_some(number)
}

/// One frame the run will take.
#[derive(Clone, Copy)]
struct Shot {
    scenario: &'static Scenario,
    variant: Variant,
}

/// Whether a scenario id matches a glob pattern (only `*` and `?`, which is all the
/// command line documents).
fn matches_pattern(pattern: &str, id: &str) -> bool {
    fn walk(pattern: &[u8], id: &[u8]) -> bool {
        match pattern.split_first() {
            None => id.is_empty(),
            Some((b'*', rest)) => (0..=id.len()).any(|skip| walk(rest, &id[skip..])),
            Some((b'?', rest)) => !id.is_empty() && walk(rest, &id[1..]),
            Some((byte, rest)) => id.first() == Some(byte) && walk(rest, &id[1..]),
        }
    }
    walk(pattern.as_bytes(), id.as_bytes())
}

/// The shots a run takes, in the order it takes them.
///
/// Ordered by window size first, so the hidden window is resized as few times as
/// possible, and by registry order inside a size, so two runs of the same command write
/// the same list in the same order.
fn plan(options: &Options) -> Result<Vec<Shot>, String> {
    let selected: Vec<&'static Scenario> = SCENARIOS
        .iter()
        .filter(|scenario| {
            options.only.is_empty()
                || options
                    .only
                    .iter()
                    .any(|pattern| matches_pattern(pattern, scenario.id))
        })
        .collect();
    if selected.is_empty() {
        return Err(format!(
            "no scenario matches {}; --list shows the ids",
            options
                .only
                .iter()
                .map(|pattern| format!("{pattern:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if options.quick {
        let kept: Vec<&'static Scenario> = selected
            .into_iter()
            .filter(|scenario| scenario.quick)
            .collect();
        if kept.is_empty() {
            return Err(
                "no scenario the quick run covers matches --only; --list marks the quick ones"
                    .to_owned(),
            );
        }
        let variant = if options.theme == ThemeFilter::Light {
            Variant::light()
        } else {
            Variant::dark()
        };
        return Ok(kept
            .into_iter()
            .map(|scenario| Shot { scenario, variant })
            .collect());
    }
    let mut shots = Vec::new();
    for scenario in selected {
        for variant in scenario.shots() {
            if !options.theme.keeps(variant.theme) {
                continue;
            }
            if !kept_size(options, variant) {
                continue;
            }
            shots.push(Shot { scenario, variant });
        }
    }
    if shots.is_empty() {
        return Err(format!(
            "the selected scenarios have no shots at theme {:?} and the sizes asked for; \
             --list shows every shot",
            options.theme.name()
        ));
    }
    shots.sort_by(|left, right| {
        (left.variant.width as u32, left.variant.height as u32)
            .cmp(&(right.variant.width as u32, right.variant.height as u32))
    });
    Ok(shots)
}

/// Whether a shot survives the `--size` filters.
fn kept_size(options: &Options, variant: Variant) -> bool {
    options.sizes.is_empty()
        || options.sizes.iter().any(|(width, height)| {
            (variant.width - width).abs() < 0.5
                && height.is_none_or(|height| (variant.height - height).abs() < 0.5)
        })
}

/// Writes the scenario registry as JSON. This is the `--list` answer, and what the
/// gallery script prints when a person asks what can be captured.
fn print_scenarios() {
    let scenarios: Vec<Value> = SCENARIOS
        .iter()
        .map(|scenario| {
            let shots: Vec<Value> = scenario
                .shots()
                .into_iter()
                .map(|variant| {
                    json!({
                        "file": format!("{}.png", variant.file_stem(scenario.id)),
                        "theme": variant.theme_name(),
                        "logical_width": f64::from(variant.width),
                        "logical_height": f64::from(variant.height),
                        "zoom_percent": variant.zoom_percent(),
                        "language": variant.language_name(),
                    })
                })
                .collect();
            json!({
                "id": scenario.id,
                "title": scenario.title,
                "category": scenario.category.id(),
                "quick": scenario.quick,
                "fixture": scenario.fixture,
                "evidence": scenario.evidence,
                "shots": shots,
            })
        })
        .collect();
    let listed = json!({
        "tool": "shellune-ui-capture",
        "schema": 1,
        "feature": "ui-capture",
        "platform": std::env::consts::OS,
        "scenarios": scenarios,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&listed).unwrap_or_default()
    );
}

/// What one shot measured about the frame it accepted.
#[cfg(target_os = "windows")]
struct Accepted {
    image: windows::Image,
    distinct_colors: usize,
    mean_luminance: f64,
    /// The colour a blank frame is filled with, so a failure can say what the system
    /// handed back instead of only that it was uniform.
    uniform_colour: String,
    pixel_crc32: u32,
    window_visible: bool,
    frames: u32,
    /// The logical size the shell had laid out for when the pixels were read.
    viewport: (f32, f32),
    scale: f64,
}

/// One shot in the manifest.
struct Record {
    file: String,
    scenario: &'static str,
    title: &'static str,
    category: &'static str,
    theme: &'static str,
    language: &'static str,
    zoom_percent: u32,
    logical_width: f32,
    logical_height: f32,
    physical_width: u32,
    physical_height: u32,
    scale_factor: f64,
    bytes: u64,
    distinct_colors: usize,
    mean_luminance: f64,
    pixel_crc32: u32,
    window_visible: bool,
    frames: u32,
    fixture: &'static str,
    evidence: &'static str,
}

impl Record {
    fn to_json(&self) -> Value {
        json!({
            "file": self.file,
            "scenario": self.scenario,
            "title": self.title,
            "category": self.category,
            "theme": self.theme,
            "language": self.language,
            "zoom_percent": self.zoom_percent,
            "logical_width": f64::from(self.logical_width),
            "logical_height": f64::from(self.logical_height),
            "physical_width": self.physical_width,
            "physical_height": self.physical_height,
            "scale_factor": self.scale_factor,
            "dpi_percent": (self.scale_factor * 100.0).round(),
            "bytes": self.bytes,
            "distinct_colors": self.distinct_colors,
            "mean_luminance": self.mean_luminance,
            "pixel_crc32": format!("{:08x}", self.pixel_crc32),
            "window_visible_during_capture": self.window_visible,
            "frames_drawn": self.frames,
            "fixture": self.fixture,
            "evidence_boundary": self.evidence,
        })
    }
}

/// One shot that could not be taken.
struct Failure {
    scenario: &'static str,
    theme: &'static str,
    size: String,
    reason: String,
}

impl Failure {
    fn shot(shot: Shot, reason: impl Into<String>) -> Self {
        Self {
            scenario: shot.scenario.id,
            theme: shot.variant.theme_name(),
            size: format!(
                "{}x{}",
                shot.variant.width as u32, shot.variant.height as u32
            ),
            reason: reason.into(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "scenario": self.scenario,
            "theme": self.theme,
            "size": self.size,
            "reason": self.reason,
        })
    }
}

/// Everything a run reports, whether it passed or failed.
struct Manifest {
    status: &'static str,
    mode: &'static str,
    planned: usize,
    started: Option<String>,
    elapsed_ms: u128,
    scale_factor: f64,
    out: String,
    records: Vec<Record>,
    failures: Vec<Failure>,
    /// Why the run stopped before every planned shot was attempted, if it did.
    stopped: Option<String>,
    /// The foreground window before the first shot and after the last one. A capture
    /// that took the focus would change it (R1).
    foreground: Option<(isize, isize)>,
    /// What a foreground change that was not this tool is, when one happened.
    ///
    /// Another window taking the foreground does not fail the run: the hidden capture
    /// window cannot cause it, and a busy desktop would otherwise never produce a
    /// gallery. The note keeps the fact in the evidence (R1).
    foreground_note: Option<String>,
}

impl Manifest {
    fn new(options: &Options, planned: usize) -> Self {
        Self {
            status: "ok",
            mode: options.mode(),
            planned,
            started: None,
            elapsed_ms: 0,
            scale_factor: 0.0,
            out: options.out.display().to_string(),
            records: Vec::new(),
            failures: Vec::new(),
            stopped: None,
            foreground: None,
            foreground_note: None,
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.status = "failed";
        self.failures.push(failure);
    }

    fn to_json(&self) -> Value {
        let records: Vec<Value> = self.records.iter().map(Record::to_json).collect();
        let failures: Vec<Value> = self.failures.iter().map(Failure::to_json).collect();
        json!({
            "tool": "shellune-ui-capture",
            "schema": 1,
            "status": self.status,
            "feature": "ui-capture",
            "platform": std::env::consts::OS,
            "mode": self.mode,
            "method": "GPU back-buffer readback of a hidden GPUI window (Window::capture_frame)",
            "window": {
                "show": false,
                "focus": false,
                "activated": false,
            },
            "method_notes": [
                "the pixels come from the hidden window's own DirectX back buffer; the screen, the desktop and other applications are never read",
                "the window is created with show:false and focus:false and is never activated, moved to the foreground or asked for input",
                "a shot is accepted only after the scenario reports its state and two consecutive readbacks are byte-identical",
            ],
            "isolation": {
                "hosts": "per-scenario temporary directory",
                "trust": "per-scenario temporary directory",
                "preferences": "per-scenario temporary directory",
                "workspace": "per-scenario temporary directory",
                "snippets": "per-scenario temporary directory",
                "credentials": "MemorySecrets (in memory only)",
                "network": "nothing dialled: no SSH connection, ProxyCommand, PTY or user command is started",
            },
            "evidence_boundary": [
                "the sessions in the shots are display-only fixtures; a PNG of a page is not proof that the page works",
                "only the Windows DirectX readback path is exercised; macOS and Linux capture is unverified",
                "the text is rendered with the fonts installed on this machine, so a pixel comparison across machines is not meaningful",
                "no layout selector is read: a scenario confirms arrival from the same model the render reads, not from where a pixel landed",
            ],
            "foreground_window": self.foreground.map(|(before, after)| json!({
                "before": before,
                "after": after,
                "unchanged": before == after,
            })),
            "foreground_note": self.foreground_note,
            "started": self.started,
            "elapsed_ms": self.elapsed_ms,
            "scale_factor": self.scale_factor,
            "out": self.out,
            "planned_shots": self.planned,
            "captured_shots": self.records.len(),
            "stopped": self.stopped,
            "shots": records,
            "failures": failures,
        })
    }
}

#[cfg(target_os = "windows")]
fn stage(message: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "shellune ui-capture: {message}");
    let _ = std::io::stderr().flush();
}

fn run(options: Options, shots: Vec<Shot>) -> i32 {
    if let Err(error) = std::fs::create_dir_all(&options.out) {
        eprintln!(
            "shellune ui-capture: cannot use {}: {error}",
            options.out.display()
        );
        return 1;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut manifest = Manifest::new(&options, shots.len());
        manifest.fail(Failure {
            scenario: "*",
            theme: options.theme.name(),
            size: "*".to_owned(),
            reason: "hidden-window capture is implemented for Windows only; macOS and Linux \
                     are unverified and this tool does not fall back to a visible window or a \
                     screen capture"
                .to_owned(),
        });
        write_manifest(&options.out, &manifest);
        eprintln!("shellune ui-capture: hidden-window capture is Windows only and unverified here");
        return 3;
    }
    #[cfg(target_os = "windows")]
    {
        run_windows(options, shots)
    }
}

#[cfg(target_os = "windows")]
fn run_windows(options: Options, shots: Vec<Shot>) -> i32 {
    // One root per run: every shot gets a directory under it, and the whole tree is
    // removed when the run ends, however it ends.
    let root = std::env::temp_dir().join("shellune-ui-capture").join("run");
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!(
            "shellune ui-capture: cannot create the isolated root {}: {error}",
            root.display()
        );
        return 1;
    }

    stage(&format!("isolated root {}", root.display()));
    let state = Arc::new(Mutex::new(RunState::default()));
    start_watchdog(state.clone(), options.clone(), shots.len(), &root);
    stage("application starting");

    let app_state = state.clone();
    let app_options = options.clone();
    let app_root = root.clone();
    Application::new()
        .with_assets(icons::Assets)
        .run(move |cx| {
            if let Err(message) = start(cx, &app_options, &app_root, shots, app_state.clone()) {
                fail(cx, &app_options, &app_state, message);
            }
        });

    let _ = std::fs::remove_dir_all(&root);
    state.lock().map_or(1, |state| state.exit_code.unwrap_or(1))
}

/// What the run has seen so far, shared by the frame callbacks, the capture task and the
/// watchdog.
#[cfg(target_os = "windows")]
#[derive(Default)]
struct RunState {
    frames: u32,
    exit_code: Option<i32>,
}

/// Ends the run with a report when the window system never lets it finish.
#[cfg(target_os = "windows")]
fn start_watchdog(state: Arc<Mutex<RunState>>, options: Options, planned: usize, root: &Path) {
    let root = root.to_path_buf();
    let budget = WATCHDOG_BASE + WATCHDOG_PER_SHOT * u32::try_from(planned).unwrap_or(0);
    std::thread::spawn(move || {
        std::thread::sleep(budget);
        if state.lock().is_ok_and(|state| state.exit_code.is_some()) {
            return;
        }
        let mut manifest = Manifest::new(&options, planned);
        manifest.elapsed_ms = budget.as_millis();
        manifest.fail(Failure {
            scenario: "*",
            theme: options.theme.name(),
            size: "*".to_owned(),
            reason: format!(
                "the run did not finish within {} s, so the hidden window stopped producing \
                 capturable frames",
                budget.as_secs()
            ),
        });
        write_manifest(&options.out, &manifest);
        let _ = std::fs::remove_dir_all(&root);
        std::process::exit(1);
    });
}

/// DirectComposition gives a window no classic redirection surface, so the tool asks
/// gpui for the plain window swap chain instead (ADR-1).
///
/// The variable is only set when the caller has not set it: an environment that cannot do
/// this has to fail in the capture step, not be silently overridden.
#[cfg(target_os = "windows")]
fn disable_direct_composition() {
    if std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_none() {
        // SAFETY: nothing else runs in this process yet; the platform reads the variable
        // while it creates windows, which happens after this call.
        unsafe { std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1") }
    }
}

#[cfg(target_os = "windows")]
fn start(
    cx: &mut App,
    options: &Options,
    root: &Path,
    shots: Vec<Shot>,
    state: Arc<Mutex<RunState>>,
) -> Result<(), String> {
    let first = shots
        .first()
        .copied()
        .ok_or_else(|| "the run has no shots".to_owned())?;
    let (min_width, min_height) = super::MIN_WINDOW_SIZE;
    let window_options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(first.variant.width), px(first.variant.height)),
        ))),
        window_min_size: Some(size(px(min_width), px(min_height))),
        titlebar: Some(TitlebarOptions {
            title: Some(product::NAME.into()),
            appears_transparent: view::custom_titlebar(Platform::current()),
            traffic_light_position: None,
        }),
        app_id: Some(product::APP_ID.to_string()),
        // The two options this tool exists to keep (R1): created hidden, and created
        // without taking the system's focus.
        show: false,
        focus: false,
        ..Default::default()
    };

    let window_root = root.to_path_buf();
    let handle = cx
        .open_window(window_options, move |window, cx| {
            let view = cx.new(|cx| ShelluneView::for_capture(cx, Workspace::new(), &window_root));
            window.focus(view.read(cx).root_focus());
            view
        })
        .map_err(|error| format!("the hidden window could not be created: {error:#}"))?;

    stage("hidden window created");
    let run_root = root.to_path_buf();
    let run_options = options.clone();
    handle
        .update(cx, move |_, window, cx| {
            // Focusing a field is part of what a shot shows; without this the hidden
            // window would become the foreground window when it did (R1).
            if let Ok(handle) = hwnd(window) {
                windows::make_non_activating(handle);
                windows::park_offscreen(handle);
            }
            arm_frame_counter(window, state.clone());
            let state = state.clone();
            let options = run_options;
            let root = run_root;
            window
                .spawn(cx, async move |wx| {
                    run_shots(wx, &options, &root, &shots, &state).await;
                })
                .detach();
        })
        .map_err(|error| format!("the capture window closed before it was used: {error:#}"))?;
    Ok(())
}

/// Counts the frames the platform actually draws.
///
/// A frame callback is one-shot, so each one arms the next: the count is then the number
/// of frames the window system delivered, not the number the tool asked for.
#[cfg(target_os = "windows")]
fn arm_frame_counter(window: &mut Window, state: Arc<Mutex<RunState>>) {
    window.on_next_frame(move |window, _cx| {
        if let Ok(mut state) = state.lock() {
            state.frames = state.frames.saturating_add(1);
        }
        arm_frame_counter(window, state);
    });
}

#[cfg(target_os = "windows")]
fn frames_drawn(state: &Arc<Mutex<RunState>>) -> u32 {
    state.lock().map_or(0, |state| state.frames)
}

/// Takes every shot, then writes the manifest and ends the process.
#[cfg(target_os = "windows")]
async fn run_shots(
    wx: &mut AsyncWindowContext,
    options: &Options,
    root: &Path,
    shots: &[Shot],
    state: &Arc<Mutex<RunState>>,
) {
    let started = Instant::now();
    // The window's own view is drawn once before the first shot: the shell chrome is the
    // same in every shot, and its images are decoded once per process. Warming it here
    // means a shot never races an image that is still being decoded.
    if let Err(error) = settle(wx, state, frames_drawn(state).saturating_add(SETTLE_FRAMES)).await {
        let (ShotError::Scenario(reason) | ShotError::Window(reason)) = error;
        stage(&format!("the warm-up frames could not be drawn: {reason}"));
    }
    let foreground_before = windows::foreground();
    let mut manifest = Manifest::new(options, shots.len());
    manifest.started = Some(now_text());
    for (index, shot) in shots.iter().copied().enumerate() {
        stage(&format!(
            "shot {}/{}/{} {}-{} at {}x{}",
            index + 1,
            shots.len(),
            shot.scenario.id,
            shot.variant.theme_name(),
            shot.variant.language_name(),
            shot.variant.width as u32,
            shot.variant.height as u32
        ));
        match capture_shot(wx, options, root, shot, state).await {
            Ok(Some(record)) => manifest.records.push(record),
            Ok(None) => {}
            Err(ShotError::Scenario(reason)) => {
                manifest.fail(Failure::shot(shot, reason));
            }
            Err(ShotError::Window(reason)) => {
                // The window itself is gone or unreadable: every later shot would fail
                // the same way, so the run stops with the reason it has.
                manifest.fail(Failure::shot(shot, reason.clone()));
                manifest.stopped = Some(reason);
                break;
            }
        }
    }
    let foreground_after = windows::foreground();
    manifest.foreground = Some((foreground_before, foreground_after));
    if foreground_after != foreground_before {
        // Only this process taking the foreground is a capture failure. Another window
        // doing it is the desktop being busy, which a hidden window cannot cause; it is
        // recorded rather than failing a run that touched nothing (R1).
        let own_window = on_window(wx, |window, _cx| hwnd(window)).ok();
        if own_window == Some(foreground_after) {
            manifest.fail(Failure {
                scenario: "*",
                theme: options.theme.name(),
                size: "*".to_owned(),
                reason: format!(
                    "the capture window took the foreground during the run ({foreground_before} -> {foreground_after}); the capture must not touch another application"
                ),
            });
        } else {
            manifest.foreground_note = Some(format!(
                "another application took the foreground during the run ({foreground_before} -> {foreground_after}); the hidden capture window was never shown"
            ));
        }
    }
    manifest.elapsed_ms = started.elapsed().as_millis();
    manifest.scale_factor = state_scale(wx);
    write_manifest(&options.out, &manifest);
    let summary = manifest.to_json();
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if manifest.status != "ok" {
        for failure in &manifest.failures {
            eprintln!(
                "shellune ui-capture: failed {} {}-{}: {}",
                failure.scenario, failure.theme, failure.size, failure.reason
            );
        }
    }
    if let Ok(mut state) = state.lock() {
        state.exit_code = Some(i32::from(manifest.status != "ok"));
    }
    let _ = wx.update(|_, cx| cx.quit());
}

/// The window's DPI scale, for the manifest's run-level line.
#[cfg(target_os = "windows")]
fn state_scale(wx: &mut AsyncWindowContext) -> f64 {
    wx.update(|window, _cx| f64::from(window.scale_factor()))
        .unwrap_or(0.0)
}

/// A step that failed, and whether the window or the scenario is to blame.
#[cfg(target_os = "windows")]
enum ShotError {
    /// The scenario never arrived, or its fixture could not be written.
    Scenario(String),
    /// The window could not be sized, painted or read.
    Window(String),
}

impl From<String> for ShotError {
    fn from(reason: String) -> Self {
        Self::Window(reason)
    }
}

/// The directory one shot's stores live in.
///
/// The name is the shot's own file stem and nothing else. It used to carry the shot's
/// position in the plan in front of it, and that number reaches the pixels: the hosts
/// page prints where its records are stored, so the same shot came out different in a
/// quick run (position 1) than in a full one (position 7) - a nine by twelve pixel
/// difference in one glyph, which reads as a change that never happened. A shot's
/// directory is named by the shot, so the plan it belongs to cannot be seen in its
/// picture.
#[cfg(any(target_os = "windows", test))]
fn shot_root_name(shot: Shot) -> String {
    shot.variant.file_stem(shot.scenario.id)
}

/// Takes one shot: fresh view, fresh isolated root, waited-for state, stable frame.
#[cfg(target_os = "windows")]
async fn capture_shot(
    wx: &mut AsyncWindowContext,
    options: &Options,
    root: &Path,
    shot: Shot,
    state: &Arc<Mutex<RunState>>,
) -> Result<Option<Record>, ShotError> {
    let shot_root = root.join(shot_root_name(shot));
    // A root that is the same on every run keeps anything the shell renders from the data
    // directory (a notice naming it, for one) out of the picture as a run-to-run difference.
    if let Err(error) = std::fs::remove_dir_all(&shot_root)
        && shot_root.exists()
    {
        return Err(ShotError::Scenario(format!(
            "the isolated root {} from an earlier run could not be cleared: {error}",
            shot_root.display()
        )));
    }
    std::fs::create_dir_all(&shot_root).map_err(|error| {
        ShotError::Scenario(format!(
            "the isolated root {} could not be created: {error}",
            shot_root.display()
        ))
    })?;

    let outcome = capture_in(wx, options, &shot_root, shot, state).await;
    // The directory goes away whether the shot worked or not: its stores are the
    // fixture, not the evidence.
    if let Err(error) = std::fs::remove_dir_all(&shot_root) {
        stage(&format!(
            "could not remove the isolated root {}: {error}",
            shot_root.display()
        ));
    }
    outcome
}

#[cfg(target_os = "windows")]
async fn capture_in(
    wx: &mut AsyncWindowContext,
    options: &Options,
    shot_root: &Path,
    shot: Shot,
    state: &Arc<Mutex<RunState>>,
) -> Result<Option<Record>, ShotError> {
    // The window takes the shot's size before anything is built in it. The layout is made
    // for the viewport, so a view or a fixture prepared while the window still had another
    // size would be photographed as the top-left of the bigger picture.
    fit_window(wx, state, shot).await?;
    if let Err(reason) = (shot.scenario.prepare)(shot_root) {
        return Err(ShotError::Scenario(reason));
    }
    install_view(wx, shot, shot_root)?;
    wait_ready(wx, shot).await?;
    let Accepted {
        image,
        distinct_colors,
        mean_luminance,
        uniform_colour,
        pixel_crc32,
        window_visible,
        frames,
        viewport,
        scale,
    } = stable_frame(wx, state).await?;
    // A notice has a timer, so the frame is accepted only if the notice is still the
    // scenario's state in it; a shot of the terminal after it expired must fail (R2).
    // Other scenes keep state that only the shot itself ends, so they are not re-read.
    if shot.scenario.id == "notification" {
        wait_ready(wx, shot).await?;
    }

    let file = format!("{}.png", shot.variant.file_stem(shot.scenario.id));
    let path = options.out.join(&file);
    let png = encode_png(image.width, image.height, &image.rgb);
    std::fs::write(&path, &png).map_err(|error| {
        ShotError::Window(format!(
            "could not write {}: {error} (the frame itself was read back)",
            path.display()
        ))
    })?;

    // The frame has to be the picture of the layout the manifest will name. A back buffer
    // that was resized while the shell still laid out for another viewport reads back as
    // the top-left of that bigger layout, which is a crop, not a shot.
    let measured = WindowSize {
        viewport,
        frame: (image.width, image.height),
        scale,
    };
    if let Some(reason) = measured.problem(shot.variant.width, shot.variant.height) {
        return Err(ShotError::Window(format!(
            "the frame is {}x{} physical pixels and cannot be called a {}x{} logical window at \
             scale {scale}: {reason}; {} was kept",
            image.width,
            image.height,
            shot.variant.width as u32,
            shot.variant.height as u32,
            path.display()
        )));
    }
    if distinct_colors < DISTINCT_COLOR_FLOOR {
        return Err(ShotError::Window(format!(
            "the frame holds only {distinct_colors} distinct colour(s) ({uniform_colour}), so \
             the GPU back buffer did not contain the page; {} was kept",
            path.display()
        )));
    }
    if window_visible {
        return Err(ShotError::Window(format!(
            "the capture window was visible while the frame was read; {} was kept",
            path.display()
        )));
    }
    Ok(Some(Record {
        file,
        scenario: shot.scenario.id,
        title: shot.scenario.title,
        category: shot.scenario.category.id(),
        theme: shot.variant.theme_name(),
        language: shot.variant.language_name(),
        zoom_percent: shot.variant.zoom_percent(),
        logical_width: shot.variant.width,
        logical_height: shot.variant.height,
        physical_width: image.width,
        physical_height: image.height,
        scale_factor: scale,
        bytes: png.len() as u64,
        distinct_colors,
        mean_luminance,
        pixel_crc32,
        window_visible,
        frames,
        fixture: shot.scenario.fixture,
        evidence: shot.scenario.evidence,
    }))
}

/// Builds the shot's shell in the window, with every store under the shot's own root.
#[cfg(target_os = "windows")]
fn install_view(
    wx: &mut AsyncWindowContext,
    shot: Shot,
    shot_root: &Path,
) -> Result<(), ShotError> {
    let root = shot_root.to_path_buf();
    let scenario = shot.scenario;
    let variant = shot.variant;
    wx.update(|window, cx| {
        let workspace = (scenario.start)();
        let view = window.replace_root(cx, move |_window, cx| {
            ShelluneView::for_capture(cx, workspace, &root)
        });
        window.focus(view.read(cx).root_focus());
        view.update(cx, |view, cx| {
            scenarios::install(scenario, variant, view, window, cx)
        })
    })
    .map_err(|error| ShotError::Window(format!("the capture window went away: {error:#}")))?
    .map_err(ShotError::Scenario)
}

/// Waits until the scenario reports its state, painting the hidden window while it waits.
///
/// A hidden window is not painted by the system, so the wait has to keep asking for a
/// frame; a fixed sleep would be a guess about how slow the machine is.
#[cfg(target_os = "windows")]
async fn wait_ready(wx: &mut AsyncWindowContext, shot: Shot) -> Result<(), ShotError> {
    let started = Instant::now();
    loop {
        // A window that went away is not a scenario that has not arrived yet: the first
        // ends the shot, the second is waited out.
        let verdict = on_window(wx, |window, cx| {
            let Some(view) = window.root::<ShelluneView>().flatten() else {
                return Err("the capture window lost its shell view".to_owned());
            };
            Ok((shot.scenario.ready)(view.read(cx), cx).err())
        })?;
        let Some(reason) = verdict else {
            return Ok(());
        };
        if started.elapsed() > SETTLE_TIMEOUT {
            return Err(ShotError::Scenario(format!(
                "the scenario never arrived within {} ms: {reason}",
                SETTLE_TIMEOUT.as_millis()
            )));
        }
        paint_once(wx)?;
        wx.background_executor().timer(SETTLE_POLL).await;
    }
}

/// Reads the frame back until two consecutive readbacks are byte-identical.
///
/// That is what "settled" means here: fonts, layout and the terminal grid have all been
/// drawn, and the next frame would be the same picture. It is a property observed on the
/// pixels, not a number of milliseconds.
#[cfg(target_os = "windows")]
async fn stable_frame(
    wx: &mut AsyncWindowContext,
    state: &Arc<Mutex<RunState>>,
) -> Result<Accepted, ShotError> {
    let started = Instant::now();
    let target = frames_drawn(state).saturating_add(SETTLE_FRAMES);
    settle(wx, state, target).await?;
    let mut previous = grab(wx, state)?;
    let mut reference = Instant::now();
    loop {
        if started.elapsed() > SETTLE_TIMEOUT {
            return Err(ShotError::Window(format!(
                "the frame never stopped changing within {} ms, so the shot cannot be called \
                 deterministic",
                SETTLE_TIMEOUT.as_millis()
            )));
        }
        // Keep the hidden window painting while the observation window runs: a hidden
        // window is not painted by the system, so waiting without asking for frames
        // would only wait.
        settle(wx, state, frames_drawn(state).saturating_add(1)).await?;
        wx.background_executor().timer(SETTLE_POLL).await;
        if reference.elapsed() < STABLE_WINDOW {
            continue;
        }
        let next = grab(wx, state)?;
        let same = previous.image.width == next.image.width
            && previous.image.height == next.image.height
            && previous.image.rgb == next.image.rgb;
        if same {
            return Ok(next);
        }
        previous = next;
        reference = Instant::now();
    }
}

/// Reads the hidden window's frame back and measures it.
#[cfg(target_os = "windows")]
fn grab(wx: &mut AsyncWindowContext, state: &Arc<Mutex<RunState>>) -> Result<Accepted, ShotError> {
    let frames = frames_drawn(state);
    on_window(wx, |window, _cx| {
        let handle = hwnd(window)?;
        // Read before the capture: the question is what the system said while the pixels
        // were taken, not after.
        let window_visible = windows::visible(handle);
        // Read while the pixels are taken: the viewport is the size the frame was laid
        // out for, and it is what tells this shot apart from a crop of a bigger layout.
        let viewport = window.viewport_size();
        let frame = window.capture_frame()?;
        let image = windows::Image {
            width: frame.width,
            height: frame.height,
            rgb: frame.rgb,
        };
        let stats = analyse(&image)?;
        Ok(Accepted {
            pixel_crc32: Crc32::of(&image.rgb),
            image,
            distinct_colors: stats.distinct_colors,
            mean_luminance: stats.mean_luminance,
            uniform_colour: stats.uniform_colour,
            window_visible,
            frames,
            viewport: (f32::from(viewport.width), f32::from(viewport.height)),
            scale: f64::from(window.scale_factor()),
        })
    })
}

/// How long one size request stands before it is asked for again.
///
/// A request is a message to the platform rather than a call that resizes the window, so
/// it has to be given a turn of the loop before a second one would mean anything.
#[cfg(target_os = "windows")]
const FIT_NUDGE: Duration = Duration::from_secs(1);

/// The two sizes a window has to agree on before a shot may be taken.
///
/// The client area the system reports is what the GPU draws into: the frame is read from
/// that back buffer. The viewport is what the shell lays out for. They are not the same
/// number, and this tool used to check only the first: a window whose back buffer had been
/// resized while the layout still believed the old size draws the top-left of the bigger
/// layout, so a 720x480 shot and a 1120x720 shot came out as one picture cut in half.
/// Comparing both is what tells a shot of a layout apart from a crop of it.
#[cfg(any(target_os = "windows", test))]
#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowSize {
    /// The logical size the shell laid out for, as width and height.
    viewport: (f32, f32),
    /// The area in physical pixels that was drawn and read back.
    frame: (u32, u32),
    /// The DPI scale in force while the shot was taken.
    scale: f64,
}

#[cfg(any(target_os = "windows", test))]
impl WindowSize {
    /// The physical pixels a logical size means at this scale.
    fn target(self, width: f32, height: f32) -> (u32, u32) {
        (
            (f64::from(width) * self.scale).round() as u32,
            (f64::from(height) * self.scale).round() as u32,
        )
    }

    /// What is still wrong with this window for a shot of `width` by `height`, if anything.
    ///
    /// A pixel of slack covers the rounding a fractional DPI scale does; anything more is
    /// the window and the layout disagreeing, which is a picture nobody asked for.
    fn problem(self, width: f32, height: f32) -> Option<String> {
        let (want_width, want_height) = self.target(width, height);
        let mut faults = Vec::new();
        if self.frame.0.abs_diff(want_width) > 1 || self.frame.1.abs_diff(want_height) > 1 {
            faults.push(format!(
                "the frame is {}x{} physical pixels, not {want_width}x{want_height}",
                self.frame.0, self.frame.1
            ));
        }
        if (self.viewport.0 - width).abs() > 0.5 || (self.viewport.1 - height).abs() > 0.5 {
            faults.push(format!(
                "the shell is laid out for a {}x{} viewport, not {width}x{height}",
                self.viewport.0, self.viewport.1
            ));
        }
        (!faults.is_empty()).then(|| faults.join("; "))
    }
}

/// Reads the window's two sizes back, from the window and from the system.
#[cfg(target_os = "windows")]
fn measure(wx: &mut AsyncWindowContext) -> Result<WindowSize, ShotError> {
    on_window(wx, |window, _cx| {
        let viewport = window.viewport_size();
        let (frame_width, frame_height) = windows::client_size(hwnd(window)?)?;
        Ok(WindowSize {
            viewport: (f32::from(viewport.width), f32::from(viewport.height)),
            frame: (frame_width.max(0) as u32, frame_height.max(0) as u32),
            scale: f64::from(window.scale_factor()),
        })
    })
}

/// Gives the hidden window the size the shot asks for, before anything is built in it.
///
/// A window that is created hidden never receives its initial placement, so it keeps the
/// size the system chose; the tool sets the size it wants and waits until the window
/// reports it, instead of assuming the window honoured it.
///
/// The size is asked for through `Window::resize`, which hands the request to the
/// platform, and never by moving the window from inside an update. That is the fix: a
/// `SetWindowPos` from inside an update delivers `WM_SIZE` re-entrantly, the nested
/// window update cannot run, and the shell keeps the layout it had - the back buffer is
/// resized, the picture is not, and the shot is a crop. Posted instead, the layout hears
/// about the new size in its own turn, which is what the viewport read back here checks.
///
/// The step ends only when the frame and the viewport both name this shot's size, and only
/// after the layout has been given frames to run at that size. A window that never gets
/// there fails the shot, with both numbers in the reason.
#[cfg(target_os = "windows")]
async fn fit_window(
    wx: &mut AsyncWindowContext,
    state: &Arc<Mutex<RunState>>,
    shot: Shot,
) -> Result<(), ShotError> {
    let width = shot.variant.width;
    let height = shot.variant.height;
    let started = Instant::now();
    let mut asked: Option<Instant> = None;
    loop {
        let measured = measure(wx)?;
        let reason = match measured.problem(width, height) {
            // Both sizes are this shot's: the frames drawn next are what makes the layout
            // the shot photographs the one made for this size.
            None => {
                settle(wx, state, frames_drawn(state).saturating_add(SETTLE_FRAMES)).await?;
                let settled = measure(wx)?;
                match settled.problem(width, height) {
                    None => return Ok(()),
                    Some(reason) => {
                        return Err(ShotError::Window(format!(
                            "the window changed size again while the frames after the resize \
                             were drawn: {reason}"
                        )));
                    }
                }
            }
            Some(reason) => reason,
        };
        // One request stands while it is answered: asking again before then would only post
        // the same message twice.
        if asked.is_none_or(|at| at.elapsed() > FIT_NUDGE) {
            on_window(wx, |window, _cx| {
                window.resize(size(px(width), px(height)));
                Ok(())
            })?;
            asked = Some(Instant::now());
        }
        if started.elapsed() > SETTLE_TIMEOUT {
            return Err(ShotError::Window(format!(
                "the hidden window did not take the {width}x{height} logical size within {} \
                 ms: {reason}",
                SETTLE_TIMEOUT.as_millis()
            )));
        }
        paint_once(wx)?;
        wx.background_executor().timer(SETTLE_POLL).await;
    }
}
/// Asks the window to paint until it has drawn `target` frames.
#[cfg(target_os = "windows")]
async fn settle(
    wx: &mut AsyncWindowContext,
    state: &Arc<Mutex<RunState>>,
    target: u32,
) -> Result<(), ShotError> {
    let started = Instant::now();
    loop {
        let drawn = frames_drawn(state);
        if drawn >= target {
            return Ok(());
        }
        if started.elapsed() > SETTLE_TIMEOUT {
            return Err(ShotError::Window(format!(
                "the hidden window drew {drawn} frame(s) in {} ms, short of the {target} needed",
                SETTLE_TIMEOUT.as_millis()
            )));
        }
        paint_once(wx)?;
        wx.background_executor().timer(SETTLE_POLL).await;
    }
}

/// Marks the window dirty and posts the paint message the platform posts for a visible
/// window. The message is handled after the closure returns, so gpui never draws while
/// the caller still holds its state.
#[cfg(target_os = "windows")]
fn paint_once(wx: &mut AsyncWindowContext) -> Result<(), ShotError> {
    on_window(wx, |window, _cx| {
        window.refresh();
        windows::post_paint(hwnd(window)?);
        Ok(())
    })?;
    Ok(())
}

/// Runs a closure against the window, turning a closed window into a readable reason.
#[cfg(target_os = "windows")]
fn on_window<T>(
    wx: &mut AsyncWindowContext,
    work: impl FnOnce(&mut Window, &mut App) -> Result<T, String>,
) -> Result<T, ShotError> {
    match wx.update(work) {
        Ok(result) => result.map_err(ShotError::Window),
        Err(error) => Err(ShotError::Window(format!(
            "the capture window went away: {error:#}"
        ))),
    }
}

#[cfg(target_os = "windows")]
fn hwnd(window: &Window) -> Result<windows::Handle, String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| format!("the capture window has no system handle: {error}"))?;
    match handle.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get() as windows::Handle),
        other => Err(format!(
            "the capture window is not a Win32 window: {other:?}"
        )),
    }
}

/// Ends the run when the tool could not even start.
#[cfg(target_os = "windows")]
fn fail(cx: &mut App, options: &Options, state: &Arc<Mutex<RunState>>, message: String) {
    let mut manifest = Manifest::new(options, 0);
    manifest.started = Some(now_text());
    manifest.fail(Failure {
        scenario: "*",
        theme: options.theme.name(),
        size: "*".to_owned(),
        reason: message.clone(),
    });
    write_manifest(&options.out, &manifest);
    eprintln!("shellune ui-capture: failed: {message}");
    if let Ok(mut state) = state.lock() {
        state.exit_code = Some(1);
    }
    cx.quit();
}

/// Writes the manifest beside the images it describes.
fn write_manifest(out: &Path, manifest: &Manifest) {
    let path = out.join(MANIFEST_FILE);
    match serde_json::to_vec_pretty(&manifest.to_json()) {
        Ok(bytes) => {
            if let Err(error) = std::fs::write(&path, bytes) {
                eprintln!(
                    "shellune ui-capture: could not write {}: {error}",
                    path.display()
                );
            }
        }
        Err(error) => eprintln!("shellune ui-capture: could not encode the manifest: {error}"),
    }
}

/// The wall clock of the run, so a gallery can say when the baseline was taken.
#[cfg(target_os = "windows")]
fn now_text() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = seconds / 86_400;
    let time = seconds % 86_400;
    // A UTC stamp in the ISO 8601 form, computed without a calendar crate: the gallery
    // only has to show a consistent, sortable time.
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time / 3_600,
        (time % 3_600) / 60,
        time % 60
    )
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
///
/// The arithmetic is platform-free, and the test that pins it has to run on every
/// platform: a Windows-only function made `cargo test` fail to compile on macOS and
/// Linux instead of failing a test.
#[cfg(any(target_os = "windows", test))]
fn civil_from_days(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// What a frame holds, in the terms the validation speaks: is it a page at all.
#[cfg(target_os = "windows")]
struct Stats {
    distinct_colors: usize,
    mean_luminance: f64,
    /// The colour a blank frame is filled with, as `#rrggbb`, so a failure can say what
    /// the system handed back instead of only that it was uniform.
    uniform_colour: String,
}

#[cfg(target_os = "windows")]
fn analyse(image: &windows::Image) -> Result<Stats, String> {
    if image.rgb.is_empty() {
        return Err("the frame came back with no pixels".to_owned());
    }
    let mut seen = std::collections::HashSet::new();
    let mut sum = 0f64;
    let mut first = [0u8; 3];
    let (pixels, _) = image.rgb.as_chunks::<3>();
    for (index, pixel) in pixels.iter().enumerate() {
        if index == 0 {
            first = *pixel;
        }
        if seen.len() < 4096 {
            seen.insert(*pixel);
        }
        sum += luminance(pixel);
    }
    let count = (image.rgb.len() / 3) as f64;
    Ok(Stats {
        distinct_colors: seen.len(),
        mean_luminance: sum / count,
        uniform_colour: format!("#{:02x}{:02x}{:02x}", first[0], first[1], first[2]),
    })
}

#[cfg(target_os = "windows")]
fn luminance(pixel: &[u8]) -> f64 {
    0.2126f64.mul_add(
        f64::from(pixel[0]),
        0.7152f64.mul_add(f64::from(pixel[1]), 0.0722 * f64::from(pixel[2])),
    ) / 255.0
}

/// A PNG of the frame, encoded here so the tool needs no image crate.
///
/// The deflate stream uses stored blocks: the job is recording a frame, not compressing
/// an image, and a stored stream cannot disagree with the pixels.
#[cfg(target_os = "windows")]
fn encode_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    // 8 bits per channel, colour type 2 (RGB), the usual deflate and filter methods.
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &header);

    let stride = width as usize * 3;
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in 0..height as usize {
        // Filter type 0: the bytes are the pixels.
        raw.push(0);
        raw.extend_from_slice(&rgb[row * stride..(row + 1) * stride]);
    }

    let mut deflate = vec![0x78, 0x01];
    let mut rest = raw.as_slice();
    loop {
        let take = rest.len().min(65_535);
        let (block, remainder) = rest.split_at(take);
        let last = remainder.is_empty();
        deflate.push(u8::from(last));
        deflate.extend_from_slice(&(block.len() as u16).to_le_bytes());
        deflate.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
        deflate.extend_from_slice(block);
        if last {
            break;
        }
        rest = remainder;
    }
    deflate.extend_from_slice(&adler32(&raw).to_be_bytes());
    png_chunk(&mut out, b"IDAT", &deflate);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(target_os = "windows")]
fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

#[cfg(target_os = "windows")]
fn adler32(data: &[u8]) -> u32 {
    let mut a: u32 = 1;
    let mut b: u32 = 0;
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

#[cfg(target_os = "windows")]
struct Crc32(u32);

#[cfg(target_os = "windows")]
impl Crc32 {
    fn new() -> Self {
        Self(0xffff_ffff)
    }

    /// The CRC of a whole buffer, as the manifest reports it: two runs of the same shot
    /// can be compared without keeping the PNG.
    fn of(data: &[u8]) -> u32 {
        let mut crc = Self::new();
        crc.update(data);
        crc.finish()
    }

    fn update(&mut self, data: &[u8]) {
        for byte in data {
            let mut value = (self.0 ^ u32::from(*byte)) & 0xff;
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xedb8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            self.0 = (self.0 >> 8) ^ value;
        }
    }

    fn finish(&self) -> u32 {
        self.0 ^ 0xffff_ffff
    }
}

/// The decision logic the tool runs without a window: what a run covers, what a shot is
/// called, and what the manifest says about it.
#[cfg(test)]
mod tool_tests {
    use super::*;
    use crate::app::theme::ThemeChoice;

    fn options(args: &[&str]) -> Options {
        let arguments: Vec<String> = args.iter().map(|argument| (*argument).to_owned()).collect();
        Options::parse(&arguments).expect("the arguments parse")
    }

    fn record() -> Record {
        Record {
            file: "home-hosts-1120x720-dark.png".to_owned(),
            scenario: "home-hosts",
            title: "首页",
            category: "home",
            theme: "dark",
            language: "en",
            zoom_percent: 100,
            logical_width: 1120.0,
            logical_height: 720.0,
            physical_width: 1120,
            physical_height: 720,
            scale_factor: 1.0,
            bytes: 4096,
            distinct_colors: 512,
            mean_luminance: 0.2,
            pixel_crc32: 0x1234_5678,
            window_visible: false,
            frames: 7,
            fixture: "six saved hosts",
            evidence: "says nothing about a live shell",
        }
    }

    #[test]
    fn a_pattern_matches_what_the_help_says_it_matches() {
        assert!(matches_pattern("home-hosts", "home-hosts"));
        assert!(matches_pattern("home-*", "home-hosts"));
        assert!(matches_pattern("*", "settings"));
        assert!(matches_pattern("home-?osts", "home-hosts"));
        assert!(!matches_pattern("home-*", "settings"));
        assert!(!matches_pattern("home-hosts", "home-hosts-2"));
        assert!(!matches_pattern("", "home-hosts"));
    }

    #[test]
    fn an_argument_the_help_does_not_list_is_refused() {
        let arguments = vec!["--nothing".to_owned()];
        assert!(Options::parse(&arguments).is_err());
        let arguments = vec!["--theme".to_owned(), "sepia".to_owned()];
        assert!(Options::parse(&arguments).is_err());
        let arguments = vec!["--size".to_owned(), "wide".to_owned()];
        assert!(Options::parse(&arguments).is_err());
        let arguments = vec!["--size".to_owned(), "0x720".to_owned()];
        assert!(Options::parse(&arguments).is_err());
        let arguments = vec!["--only".to_owned()];
        assert!(Options::parse(&arguments).is_err());
    }

    #[test]
    fn the_flags_are_read_as_they_are_written() {
        let parsed = options(&[
            "--quick",
            "--theme",
            "light",
            "--size",
            "1120x720",
            "--size",
            "720",
            "--only",
            "home-*,settings",
            "--out",
            "somewhere/else",
        ]);
        assert!(parsed.quick);
        assert_eq!(parsed.theme, ThemeFilter::Light);
        assert_eq!(parsed.sizes, vec![(1120.0, Some(720.0)), (720.0, None)]);
        assert_eq!(
            parsed.only,
            vec!["home-*".to_owned(), "settings".to_owned()]
        );
        assert_eq!(parsed.mode(), "quick");
        assert_eq!(parsed.out, PathBuf::from("somewhere/else"));
    }

    #[test]
    fn the_quick_run_covers_the_main_pages_in_dark_only() {
        let parsed = options(&["--quick"]);
        assert_eq!(parsed.theme, ThemeFilter::Dark);
        let shots = plan(&parsed).expect("the quick run has shots");
        let quick: Vec<&str> = SCENARIOS
            .iter()
            .filter(|scenario| scenario.quick)
            .map(|scenario| scenario.id)
            .collect();
        assert!(!quick.is_empty());
        assert_eq!(shots.len(), quick.len(), "one shot per quick page");
        for shot in &shots {
            assert!(
                shot.scenario.quick,
                "{} is not a quick page",
                shot.scenario.id
            );
            assert_eq!(shot.variant.theme, ThemeChoice::Dark);
            assert_eq!(shot.variant.width, crate::app::DEFAULT_WINDOW_SIZE.0);
        }
        for id in quick {
            assert!(
                shots.iter().any(|shot| shot.scenario.id == id),
                "the quick run left out {id}"
            );
        }
    }

    #[test]
    fn a_full_run_shoots_every_scenario_in_both_palettes() {
        let parsed = options(&[]);
        let shots = plan(&parsed).expect("a full run has shots");
        let expected: usize = SCENARIOS
            .iter()
            .map(|scenario| scenario.shots().len())
            .sum();
        assert_eq!(shots.len(), expected);
        for scenario in SCENARIOS {
            for theme in [ThemeChoice::Dark, ThemeChoice::Light] {
                assert!(
                    shots
                        .iter()
                        .any(|shot| shot.scenario.id == scenario.id && shot.variant.theme == theme),
                    "{} is missing its {theme:?} shot",
                    scenario.id
                );
            }
        }
        // No shot is ever taken in the palette the machine happens to be in.
        assert!(
            shots
                .iter()
                .all(|shot| shot.variant.theme != ThemeChoice::System)
        );
    }

    #[test]
    fn the_theme_and_size_flags_narrow_the_matrix_without_inventing_shots() {
        let light = plan(&options(&["--theme", "light"])).expect("the light run has shots");
        assert!(
            light
                .iter()
                .all(|shot| shot.variant.theme == ThemeChoice::Light)
        );
        assert!(!light.is_empty());

        let smallest = plan(&options(&["--size", "720x480"])).expect("the smallest size has shots");
        assert!(!smallest.is_empty());
        assert!(smallest.iter().all(|shot| {
            shot.variant.width == crate::app::MIN_WINDOW_SIZE.0
                && shot.variant.height == crate::app::MIN_WINDOW_SIZE.1
        }));

        let width_only = plan(&options(&["--size", "1120"])).expect("the width has shots");
        assert!(
            width_only
                .iter()
                .all(|shot| shot.variant.width == crate::app::DEFAULT_WINDOW_SIZE.0)
        );

        // A size no scenario is shot in is a mistake worth reporting, not an empty run.
        assert!(plan(&options(&["--size", "4096x2160"])).is_err());
    }

    #[test]
    fn only_narrows_to_the_named_scenarios_and_says_so_when_nothing_matches() {
        let shots = plan(&options(&["--only", "home-*"])).expect("the pattern matches");
        assert!(!shots.is_empty());
        assert!(
            shots
                .iter()
                .all(|shot| shot.scenario.id.starts_with("home-"))
        );

        let both =
            plan(&options(&["--only", "settings,terminal-local"])).expect("the list matches");
        let mut ids: Vec<&str> = both.iter().map(|shot| shot.scenario.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids, vec!["settings", "terminal-local"]);

        // A typo must not quietly produce a run of something else.
        assert!(plan(&options(&["--only", "nope-*"])).is_err());
        // Quick mode only covers the pages it has shots for.
        assert!(plan(&options(&["--quick", "--only", "overlay-*"])).is_err());
    }

    #[test]
    fn the_plan_is_ordered_by_window_size_so_the_window_is_resized_once() {
        let shots = plan(&options(&[])).expect("a full run has shots");
        let sizes: Vec<(u32, u32)> = shots
            .iter()
            .map(|shot| (shot.variant.width as u32, shot.variant.height as u32))
            .collect();
        let mut sorted = sizes.clone();
        sorted.sort_unstable();
        assert_eq!(sizes, sorted, "shots are grouped by window size");
        // And two runs of the same command produce the same list, in the same order.
        let again = plan(&options(&[])).expect("a full run has shots");
        let left: Vec<(String, &'static str)> = shots
            .iter()
            .map(|shot| (shot.scenario.id.to_owned(), shot.variant.theme_name()))
            .collect();
        let right: Vec<(String, &'static str)> = again
            .iter()
            .map(|shot| (shot.scenario.id.to_owned(), shot.variant.theme_name()))
            .collect();
        assert_eq!(left, right);
    }

    #[test]
    fn a_shot_lives_in_the_same_directory_whichever_plan_takes_it() {
        let quick = plan(&options(&["--quick"])).expect("the quick run has shots");
        let full = plan(&options(&[])).expect("a full run has shots");
        let only = plan(&options(&["--only", "home-hosts"])).expect("the pattern matches");
        // The hosts page prints the path of its store, so the store's directory is part of
        // the picture: the same shot has to reach the same directory whether a quick, an
        // --only or a full run takes it, or two runs of one build disagree about pixels.
        let wanted = "home-hosts-1120x720-dark";
        let named = |shots: &[Shot]| -> Vec<String> {
            let names: Vec<String> = shots
                .iter()
                .copied()
                .map(shot_root_name)
                .filter(|name| name == wanted)
                .collect();
            assert_eq!(names.len(), 1, "the shot is taken {wanted} once per run");
            names
        };
        assert_eq!(named(&quick), named(&full), "quick and full disagree");
        assert_eq!(named(&only), named(&full), "--only and full disagree");
        // And no two shots of a run share a directory: a name that did would let one
        // shot's fixture stand in for another's.
        for shots in [&quick, &full] {
            let mut names: Vec<String> = shots.iter().copied().map(shot_root_name).collect();
            let total = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), total, "two shots share a directory");
        }
    }

    #[test]
    fn a_file_name_says_theme_size_text_and_language() {
        let plain = Variant::dark();
        assert_eq!(plain.file_stem("home-hosts"), "home-hosts-1120x720-dark");
        let light = Variant::light();
        assert_eq!(light.file_stem("settings"), "settings-1120x720-light");
        let named = Variant::dark().smallest().comfortable().chinese();
        // The private builders are used through a scenario below; this asserts the
        // spelling of each modifier instead.
        assert_eq!(named.zoom_percent(), 115);
        assert_eq!(named.language_name(), "zh");
        assert_eq!(named.theme_name(), "dark");
        assert_eq!(named.width, crate::app::MIN_WINDOW_SIZE.0);
        let expected = format!(
            "terminal-local-{}x{}-dark-z115-zh",
            crate::app::MIN_WINDOW_SIZE.0 as u32,
            crate::app::MIN_WINDOW_SIZE.1 as u32
        );
        assert_eq!(named.file_stem("terminal-local"), expected);
    }

    #[test]
    fn the_manifest_names_every_fact_a_shot_was_drawn_with() {
        let mut manifest = Manifest::new(&options(&["--quick"]), 3);
        manifest.records.push(record());
        let json = manifest.to_json();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["mode"], "quick");
        assert_eq!(json["window"]["show"], false);
        assert_eq!(json["window"]["focus"], false);
        assert!(
            json["isolation"]["credentials"]
                .as_str()
                .is_some_and(|text| text.contains("Memory"))
        );
        assert!(
            json["evidence_boundary"]
                .as_array()
                .is_some_and(|lines| lines.len() >= 3)
        );
        let shot = &json["shots"][0];
        for key in [
            "theme",
            "language",
            "zoom_percent",
            "logical_width",
            "logical_height",
            "physical_width",
            "physical_height",
            "scale_factor",
            "fixture",
            "evidence_boundary",
            "pixel_crc32",
            "window_visible_during_capture",
        ] {
            assert!(!shot[key].is_null(), "the shot has no {key}");
        }
        assert_eq!(shot["theme"], "dark");
        assert_eq!(shot["language"], "en");
        assert_eq!(shot["dpi_percent"], 100.0);
        assert_eq!(shot["pixel_crc32"], "12345678");
    }

    #[test]
    fn a_failed_run_says_which_shot_failed_and_why() {
        let parsed = options(&["--quick"]);
        let shots = plan(&parsed).expect("the quick run has shots");
        let mut manifest = Manifest::new(&parsed, shots.len());
        manifest.fail(Failure::shot(shots[0], "the scenario never arrived"));
        let json = manifest.to_json();
        assert_eq!(json["status"], "failed");
        assert_eq!(json["planned_shots"], shots.len());
        assert_eq!(json["captured_shots"], 0);
        assert_eq!(json["failures"][0]["scenario"], shots[0].scenario.id);
        assert_eq!(json["failures"][0]["reason"], "the scenario never arrived");
    }

    #[test]
    fn the_run_stamp_is_a_sortable_utc_time() {
        // 2026-11-12 is 20767 days after the epoch; the stamp has to be parseable and
        // ordered, which is all the gallery asks of it.
        assert_eq!(civil_from_days(20_769), (2026, 11, 12));
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
    }
}

/// The pixel decisions the tool runs on the frames it read.
#[cfg(all(test, target_os = "windows"))]
mod frame_tests {
    use super::*;

    fn image(width: u32, height: u32, pixels: impl Fn(u32, u32) -> [u8; 3]) -> windows::Image {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(&pixels(x, y));
            }
        }
        windows::Image { width, height, rgb }
    }

    #[test]
    fn a_uniform_frame_is_reported_as_blank_with_its_colour() {
        let blank = image(4, 3, |_, _| [0xff, 0xff, 0xff]);
        let stats = analyse(&blank).expect("a frame with pixels is analysed");
        assert_eq!(stats.distinct_colors, 1);
        assert!(stats.distinct_colors < DISTINCT_COLOR_FLOOR);
        assert_eq!(stats.uniform_colour, "#ffffff");
        assert!((stats.mean_luminance - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_drawn_frame_clears_the_colour_floor_a_blank_one_cannot() {
        let drawn = image(16, 16, |x, y| [x as u8, y as u8, 0x40]);
        let stats = analyse(&drawn).expect("a frame with pixels is analysed");
        assert_eq!(stats.distinct_colors, 256);
        assert!(stats.distinct_colors >= DISTINCT_COLOR_FLOOR);
    }

    #[test]
    fn the_same_pixels_give_the_same_checksum() {
        let frame = image(8, 8, |x, y| [x as u8, y as u8, 0x40]);
        let other = image(8, 8, |x, y| [x as u8, y as u8, 0x40]);
        assert_eq!(Crc32::of(&frame.rgb), Crc32::of(&other.rgb));
        let changed = image(8, 8, |x, y| [x as u8, y as u8, 0x41]);
        assert_ne!(Crc32::of(&frame.rgb), Crc32::of(&changed.rgb));
    }

    #[test]
    fn the_encoded_file_is_a_png_of_the_frame_it_was_given() {
        let frame = image(3, 2, |x, y| [x as u8, y as u8, 0x7f]);
        let png = encode_png(frame.width, frame.height, &frame.rgb);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

        let mut offset = 8;
        let mut kinds = Vec::new();
        let mut idat = Vec::new();
        while offset < png.len() {
            let length = u32::from_be_bytes(png[offset..offset + 4].try_into().expect("length"));
            let kind = &png[offset + 4..offset + 8];
            let data = &png[offset + 8..offset + 8 + length as usize];
            let crc = u32::from_be_bytes(
                png[offset + 8 + length as usize..offset + 12 + length as usize]
                    .try_into()
                    .expect("crc"),
            );
            let mut check = Crc32::new();
            check.update(kind);
            check.update(data);
            assert_eq!(crc, check.finish(), "chunk {kind:?} carries a wrong crc");
            kinds.push(kind.to_vec());
            if kind == b"IDAT" {
                idat.extend_from_slice(data);
            }
            offset += 12 + length as usize;
        }
        assert_eq!(
            kinds,
            [b"IHDR".to_vec(), b"IDAT".to_vec(), b"IEND".to_vec()]
        );

        // The deflate stream is a zlib header and stored blocks; undoing them has to give
        // back one filter byte and the row's pixels per scan line.
        assert_eq!(idat[..2], [0x78, 0x01]);
        let mut raw = Vec::new();
        let mut at = 2;
        loop {
            let header = idat[at];
            let length = u16::from_le_bytes([idat[at + 1], idat[at + 2]]) as usize;
            raw.extend_from_slice(&idat[at + 5..at + 5 + length]);
            at += 5 + length;
            if header == 1 {
                break;
            }
        }
        assert_eq!(
            raw,
            vec![
                // First row: filter byte, then x, y, 0x7f for every pixel.
                0, 0, 0, 0x7f, 1, 0, 0x7f, 2, 0, 0x7f, // Second row: y is 1 now.
                0, 0, 1, 0x7f, 1, 1, 0x7f, 2, 1, 0x7f,
            ]
        );
        let stored = u32::from_be_bytes(idat[at..at + 4].try_into().expect("adler"));
        assert_eq!(stored, adler32(&raw));
    }
}
