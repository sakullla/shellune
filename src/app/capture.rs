//! The hidden-window capture probe (R1, R11).
//!
//! This is the first, smallest capture entry: it proves that the real Shellune view can
//! be drawn in a window the user never sees and read back as real pixels. Everything
//! here exists to answer that one question honestly, so it fails loudly rather than
//! falling back:
//!
//! * the window is opened with `show: false` and `focus: false` and is never shown,
//!   activated, moved to the foreground or asked for input;
//! * the pixels come from the hidden window's own DirectX back buffer through the
//!   vendored GPUI readback; the screen, the desktop and other applications are never
//!   read;
//! * every store is redirected to a temporary directory and credentials live in memory,
//!   so a run cannot read or write the developer's hosts, trust file or keychain;
//! * a black, empty, wrong-sized or uniform image, a failed render or a failed write
//!   ends the process non-zero with a report beside the images it did produce.
//!
//! It is a probe, not the capture tool: scenarios, filters, the manifest and the gallery
//! belong to the next task. What it prints and writes here is the machine-readable
//! result the caller can act on.
//!
//! `text
//! cargo run --features ui-capture -- --ui-capture --out target/ui-capture/probe
//! `

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

// Everything below is the Windows probe: a platform without the hidden-window capture
// still parses the same arguments, and answers with the one report that cannot be
// mistaken for a rendering result.
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
use super::theme::ThemeChoice;
#[cfg(target_os = "windows")]
use super::view;
#[cfg(target_os = "windows")]
use super::workspace::Workspace;

#[cfg(target_os = "windows")]
mod windows;

/// The flag that sends the binary to the probe instead of the application.
const FLAG: &str = "--ui-capture";

const USAGE: &str = "\
usage: shellune --ui-capture [--out <directory>] [--size <width>x<height>]

  --out <directory>     where the PNGs, their report and the run's diagnostics go
                        (default: target/ui-capture/probe)
  --size <width>x<height>
                        the window's logical client size (default: the design window,
                        1120x720)

The probe captures the real home page twice - dark, then light - in a window that is
never shown, and exits non-zero with a report when the render could not be read back.";

/// Frames the probe lets the shell draw before it reads pixels, and how long it waits
/// for the platform to deliver them at all.
const SETTLE_FRAMES: u32 = 3;
const SETTLE_TIMEOUT: Duration = Duration::from_secs(20);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// How long the whole run may take before the watchdog ends it with a report.
const WATCHDOG: Duration = Duration::from_secs(120);

/// A real page is not one colour; this is the floor below which the image is not a page.
const DISTINCT_COLOR_FLOOR: usize = 16;

/// How much of the page has to change between the two palettes for the theme to count
/// as reaching the pixels.
const THEME_PIXEL_FLOOR: f64 = 0.05;

/// Entry point. Returns the process exit status: 0 for a probe that proved itself,
/// 1 for one that did not, 2 for a bad invocation, 3 for an unsupported platform.
pub fn main(arguments: Vec<String>) -> i32 {
    let options = match Options::parse(&arguments) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("shellune ui-capture: {message}\n{USAGE}");
            return 2;
        }
    };
    run(options)
}

/// Whether these arguments ask for the probe.
#[must_use]
pub fn invoked(arguments: &[String]) -> bool {
    arguments.iter().any(|argument| argument == FLAG)
}

/// What one probe run was asked to do.
#[derive(Clone, Debug)]
struct Options {
    width: f32,
    height: f32,
    out: PathBuf,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let (width, height) = super::DEFAULT_WINDOW_SIZE;
        let mut options = Self {
            width,
            height,
            out: PathBuf::from("target").join("ui-capture").join("probe"),
        };
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
                "--size" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| "--size needs <width>x<height>".to_owned())?;
                    let (w, h) = value
                        .split_once(['x', 'X'])
                        .ok_or_else(|| format!("--size wants <width>x<height>, not {value:?}"))?;
                    options.width = w
                        .parse::<f32>()
                        .map_err(|_| format!("--size width is not a number: {w:?}"))?;
                    options.height = h
                        .parse::<f32>()
                        .map_err(|_| format!("--size height is not a number: {h:?}"))?;
                    if !(options.width.is_finite() && options.height.is_finite())
                        || options.width <= 0.0
                        || options.height <= 0.0
                    {
                        return Err(format!("--size needs positive numbers, not {value:?}"));
                    }
                }
                "--help" | "-h" => return Err("help requested".to_owned()),
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(options)
    }
}

/// What the run has seen so far, shared between the frame callbacks, the capture task
/// and the watchdog.
#[cfg(target_os = "windows")]
#[derive(Default)]
struct ProbeState {
    frames: u32,
    exit_code: Option<i32>,
    /// The foreground window right after the hidden window took its size, and right
    /// after the frames were read. Sampled so a focus change can be pinned on a step.
    foreground_after_fit: Option<isize>,
    foreground_after_capture: Option<isize>,
}

/// One captured image and what was measured on it.
struct Capture {
    theme: &'static str,
    path: PathBuf,
    width: u32,
    height: u32,
    bytes: u64,
    distinct_colors: usize,
    mean_luminance: f64,
    window_visible: bool,
}

/// Everything the report says, whether the run passed or failed.
#[derive(Default)]
struct Report {
    status: &'static str,
    error: Option<String>,
    logical_width: f32,
    logical_height: f32,
    scale_factor: f64,
    frames: u32,
    captures: Vec<Capture>,
    theme_pixel_ratio: Option<f64>,
    foreground_before: isize,
    foreground_after_fit: Option<isize>,
    foreground_after_capture: Option<isize>,
    elapsed_ms: u128,
}

impl Report {
    fn to_json(&self, options: &Options) -> Value {
        let captures: Vec<Value> = self
            .captures
            .iter()
            .map(|capture| {
                json!({
                    "theme": capture.theme,
                    "path": capture.path.display().to_string(),
                    "width": capture.width,
                    "height": capture.height,
                    "bytes": capture.bytes,
                    "distinct_colors": capture.distinct_colors,
                    "mean_luminance": capture.mean_luminance,
                    "window_visible_during_capture": capture.window_visible,
                })
            })
            .collect();
        json!({
            "probe": "hidden-window-gpu-readback",
            "schema": 1,
            "status": self.status,
            "error": self.error,
            "feature": "ui-capture",
            "platform": std::env::consts::OS,
            "method": "DirectX back-buffer readback from the hidden GPUI renderer",
            "window": {
                "show": false,
                "focus": false,
                "logical_width": self.logical_width,
                "logical_height": self.logical_height,
                "scale_factor": self.scale_factor,
                "frames_drawn": self.frames,
            },
            "theme_pixel_ratio": self.theme_pixel_ratio,
            "foreground_window": {
                "before": self.foreground_before,
                "after_fit": self.foreground_after_fit,
                "after_capture": self.foreground_after_capture,
            },
            "isolation": {
                "hosts": "temporary directory",
                "trust": "temporary directory",
                "preferences": "temporary directory",
                "credentials": "memory only",
            },
            "output": options.out.display().to_string(),
            "elapsed_ms": self.elapsed_ms,
            "captures": captures,
        })
    }
}

/// Prints one progress line to stderr, so a run that hangs says where it stopped.
///
/// The probe is the only writer of this stream, and it is how a caller with no console
/// (the binary is a window subsystem program) still sees where a run ended up.
#[cfg(target_os = "windows")]
fn stage(message: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "shellune ui-capture: {message}");
    let _ = std::io::stderr().flush();
}

fn run(options: Options) -> i32 {
    if let Err(error) = std::fs::create_dir_all(&options.out) {
        eprintln!(
            "shellune ui-capture: cannot use {}: {error}",
            options.out.display()
        );
        return 1;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let report = Report {
            status: "failed",
            error: Some(
                "hidden-window capture is implemented for Windows only; macOS and Linux are \
                 unverified and this probe does not fall back to a visible window or a screen \
                 capture"
                    .to_owned(),
            ),
            logical_width: options.width,
            logical_height: options.height,
            ..Report::default()
        };
        write_report(&options.out, &report, &options);
        eprintln!("shellune ui-capture: hidden-window capture is Windows only and unverified here");
        return 3;
    }
    #[cfg(target_os = "windows")]
    {
        run_windows(options)
    }
}
#[cfg(target_os = "windows")]
fn run_windows(options: Options) -> i32 {
    // A run must not touch the developer's data directory, so every store is redirected
    // before the first window exists.
    let root = std::env::temp_dir().join(format!(
        "shellune-ui-capture-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    if let Err(error) = std::fs::create_dir_all(&root) {
        eprintln!(
            "shellune ui-capture: cannot create the isolated root {}: {error}",
            root.display()
        );
        return 1;
    }

    stage(&format!("isolated root {}", root.display()));
    let state = Arc::new(Mutex::new(ProbeState::default()));
    start_watchdog(state.clone(), options.clone(), &root);
    stage("application starting");

    let app_state = state.clone();
    let app_options = options.clone();
    let app_root = root.clone();
    Application::new()
        .with_assets(icons::Assets)
        .run(move |cx| {
            if let Err(message) = start(cx, &app_options, &app_root, app_state.clone()) {
                fail(cx, &app_options, &app_state, message);
            }
        });

    let _ = std::fs::remove_dir_all(&root);
    state.lock().map_or(1, |state| state.exit_code.unwrap_or(1))
}

/// Ends the run with a report when the window system never lets the probe finish.
///
/// A hidden window that never paints would otherwise leave the process waiting forever;
/// this keeps the failure observable and non-zero.
#[cfg(target_os = "windows")]
fn start_watchdog(state: Arc<Mutex<ProbeState>>, options: Options, root: &Path) {
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        std::thread::sleep(WATCHDOG);
        if state.lock().is_ok_and(|state| state.exit_code.is_some()) {
            return;
        }
        let report = Report {
            status: "failed",
            error: Some(format!(
                "the probe did not finish within {} s, so the hidden window never produced a \
                 capturable frame",
                WATCHDOG.as_secs()
            )),
            logical_width: options.width,
            logical_height: options.height,
            frames: state.lock().map_or(0, |state| state.frames),
            ..Report::default()
        };
        write_report(&options.out, &report, &options);
        let _ = std::fs::remove_dir_all(&root);
        std::process::exit(1);
    });
}

/// DirectComposition gives a window no classic redirection surface for `PrintWindow` to
/// read, so the probe asks gpui for the plain window swap chain instead (ADR-1).
///
/// The variable is only set when the caller has not set it: an environment that cannot do
/// this has to fail in the capture step, not be silently overridden.
#[cfg(target_os = "windows")]
fn disable_direct_composition() {
    if std::env::var_os("GPUI_DISABLE_DIRECT_COMPOSITION").is_none() {
        // SAFETY: nothing else runs in this process yet; the platform reads the variable
        // while it creates windows, which happens after this call.
        unsafe { std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "1") };
    }
}

#[cfg(target_os = "windows")]
fn start(
    cx: &mut App,
    options: &Options,
    root: &Path,
    state: Arc<Mutex<ProbeState>>,
) -> Result<(), String> {
    disable_direct_composition();
    let foreground_before = windows::foreground();
    let (min_width, min_height) = super::MIN_WINDOW_SIZE;
    let window_options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.0), px(0.0)),
            size(px(options.width), px(options.height)),
        ))),
        window_min_size: Some(size(px(min_width), px(min_height))),
        titlebar: Some(TitlebarOptions {
            title: Some(product::NAME.into()),
            appears_transparent: view::custom_titlebar(Platform::current()),
            traffic_light_position: None,
        }),
        app_id: Some(product::APP_ID.to_string()),
        // The two options this probe exists to prove (R1): created hidden, and created
        // without taking the system's focus.
        show: false,
        focus: false,
        ..Default::default()
    };

    let view_root = root.to_path_buf();
    let handle = cx
        .open_window(window_options, move |window, cx| {
            let view = cx.new(|cx| ShelluneView::for_capture(cx, Workspace::new(), &view_root));
            // gpui's own focus, not the system's: it decides which element receives keys
            // and which focus ring a frame draws, and it cannot reach another application.
            window.focus(view.read(cx).root_focus());
            view
        })
        .map_err(|error| format!("the hidden window could not be created: {error:#}"))?;

    stage("hidden window created");
    let task_options = options.clone();
    handle
        .update(cx, move |_, window, cx| {
            arm_frame_counter(window, state.clone());
            let state = state.clone();
            let options = task_options;
            window
                .spawn(cx, async move |wx| {
                    probe(wx, &options, &state, foreground_before).await;
                })
                .detach();
        })
        .map_err(|error| format!("the capture window closed before it was captured: {error:#}"))?;
    Ok(())
}

/// Counts the frames the platform actually draws.
///
/// A frame callback is one-shot, so each one arms the next: the count is then the number
/// of frames the window system delivered, not the number the probe asked for.
#[cfg(target_os = "windows")]
fn arm_frame_counter(window: &mut Window, state: Arc<Mutex<ProbeState>>) {
    window.on_next_frame(move |window, _cx| {
        if let Ok(mut state) = state.lock() {
            state.frames = state.frames.saturating_add(1);
        }
        arm_frame_counter(window, state);
    });
}

#[cfg(target_os = "windows")]
fn frames_drawn(state: &Arc<Mutex<ProbeState>>) -> u32 {
    state.lock().map_or(0, |state| state.frames)
}

/// The whole run: settle, capture dark, capture light, and report what happened.
#[cfg(target_os = "windows")]
async fn probe(
    wx: &mut AsyncWindowContext,
    options: &Options,
    state: &Arc<Mutex<ProbeState>>,
    foreground_before: isize,
) {
    let started = Instant::now();
    let mut report = Report {
        logical_width: options.width,
        logical_height: options.height,
        foreground_before,
        ..Report::default()
    };
    let outcome = run_probe(wx, options, state, &mut report).await;
    report.elapsed_ms = started.elapsed().as_millis();
    report.frames = frames_drawn(state);
    report.foreground_after_fit = state
        .lock()
        .ok()
        .and_then(|state| state.foreground_after_fit);
    report.foreground_after_capture = Some(windows::foreground());
    if let Ok(mut state) = state.lock() {
        state.foreground_after_capture = Some(windows::foreground());
    }
    match outcome {
        Ok(()) => report.status = "ok",
        Err(message) => {
            report.status = "failed";
            report.error = Some(message);
        }
    }
    write_report(&options.out, &report, options);
    println!(
        "{}",
        serde_json::to_string_pretty(&report.to_json(options)).unwrap_or_default()
    );
    if report.status != "ok" {
        eprintln!(
            "shellune ui-capture: failed: {}",
            report.error.as_deref().unwrap_or("unknown reason")
        );
    }
    if let Ok(mut state) = state.lock() {
        state.exit_code = Some(i32::from(report.status != "ok"));
    }
    let _ = wx.update(|_, cx| cx.quit());
}

#[cfg(target_os = "windows")]
async fn run_probe(
    wx: &mut AsyncWindowContext,
    options: &Options,
    state: &Arc<Mutex<ProbeState>>,
    report: &mut Report,
) -> Result<(), String> {
    fit_window(wx, options).await?;
    if let Ok(mut state) = state.lock() {
        state.foreground_after_fit = Some(windows::foreground());
    }
    let (dark, dark_image) = grab(wx, options, state, ThemeChoice::Dark, "dark", report).await?;
    report.captures.push(dark);
    let (light, light_image) =
        grab(wx, options, state, ThemeChoice::Light, "light", report).await?;
    report.captures.push(light);

    stage("both frames captured");
    let ratio = pixel_difference(&dark_image, &light_image)?;
    report.theme_pixel_ratio = Some(ratio);
    if ratio < THEME_PIXEL_FLOOR {
        return Err(format!(
            "the two palettes changed only {:.2}% of the pixels (floor {:.0}%), so the theme is \
             not reaching the rendered frame",
            ratio * 100.0,
            THEME_PIXEL_FLOOR * 100.0
        ));
    }

    let visible: Vec<&str> = report
        .captures
        .iter()
        .filter(|capture| capture.window_visible)
        .map(|capture| capture.theme)
        .collect();
    if !visible.is_empty() {
        return Err(format!(
            "the capture window was reported visible while {} was captured",
            visible.join(", ")
        ));
    }
    let foreground_after = windows::foreground();
    if foreground_after != report.foreground_before {
        return Err(format!(
            "the foreground window changed during the run ({} -> {foreground_after}); the probe \
             must not touch another application",
            report.foreground_before
        ));
    }
    Ok(())
}

/// Gives the hidden window the client area the run asked for.
///
/// A window that is created hidden never receives its initial placement, so it keeps the
/// size the system chose; the probe sets the size it wants and waits until the system
/// reports it, instead of assuming the window honoured it.
#[cfg(target_os = "windows")]
async fn fit_window(wx: &mut AsyncWindowContext, options: &Options) -> Result<(), String> {
    stage("sizing the hidden window");
    let started = Instant::now();
    loop {
        let measured = on_window(wx, |window, _cx| {
            let scale = f64::from(window.scale_factor());
            let handle = hwnd(window)?;
            let (width, height) = windows::client_size(handle)?;
            let expected_width = (f64::from(options.width) * scale).round() as i32;
            let expected_height = (f64::from(options.height) * scale).round() as i32;
            if width.abs_diff(expected_width) > 1 || height.abs_diff(expected_height) > 1 {
                windows::resize_client(handle, expected_width, expected_height)?;
            }
            Ok((width, height, scale))
        })?;
        let (width, height, scale) = measured;
        let expected_width = (f64::from(options.width) * scale).round() as i32;
        let expected_height = (f64::from(options.height) * scale).round() as i32;
        if width.abs_diff(expected_width) <= 1 && height.abs_diff(expected_height) <= 1 {
            return Ok(());
        }
        if started.elapsed() > SETTLE_TIMEOUT {
            let logical_width = options.width;
            let logical_height = options.height;
            return Err(format!(
                "the hidden window is {width}x{height} physical pixels after {} ms; a \
                 {logical_width}x{logical_height} logical window at scale {scale} should be \
                 {expected_width}x{expected_height}",
                SETTLE_TIMEOUT.as_millis()
            ));
        }
        wx.background_executor().timer(SETTLE_POLL).await;
    }
}

/// Switches the palette, waits for frames drawn in it, reads the client area and writes it
/// as a PNG.
#[cfg(target_os = "windows")]
async fn grab(
    wx: &mut AsyncWindowContext,
    options: &Options,
    state: &Arc<Mutex<ProbeState>>,
    choice: ThemeChoice,
    name: &'static str,
    report: &mut Report,
) -> Result<(Capture, windows::Image), String> {
    stage(&format!("capturing {name}"));
    set_theme(wx, choice)?;
    let target = frames_drawn(state).saturating_add(SETTLE_FRAMES);
    settle(wx, state, target).await?;

    let path = options.out.join(format!(
        "home-{}x{}-{name}.png",
        options.width as u32, options.height as u32
    ));
    let capture_path = path.clone();
    let width = options.width;
    let height = options.height;
    let (capture, image) = on_window(wx, move |window, _cx| {
        let handle = hwnd(window)?;
        // Read before the capture: the question is what the system said while the pixels
        // were taken, not after.
        let window_visible = windows::visible(handle);
        let frame = window.capture_frame()?;
        let image = windows::Image {
            width: frame.width,
            height: frame.height,
            rgb: frame.rgb,
        };
        let stats = analyse(&image)?;

        let scale = f64::from(window.scale_factor());
        report.scale_factor = scale;
        let expected_width = (f64::from(width) * scale).round() as u32;
        let expected_height = (f64::from(height) * scale).round() as u32;
        let near = |want: u32, got: u32| want.abs_diff(got) <= 1;

        // Written before it is judged: a failed run has to leave the image that failed.
        let png = encode_png(image.width, image.height, &image.rgb);
        std::fs::write(&capture_path, &png).map_err(|error| {
            format!(
                "could not write {}: {error} (the frame itself was read back)",
                capture_path.display()
            )
        })?;

        if !near(expected_width, image.width) || !near(expected_height, image.height) {
            return Err(format!(
                "the frame is {}x{} physical pixels but a {width}x{height} logical window at \
                 scale {scale} should be {expected_width}x{expected_height}; {} was kept",
                image.width,
                image.height,
                capture_path.display()
            ));
        }
        if stats.distinct_colors < DISTINCT_COLOR_FLOOR {
            return Err(format!(
                "the frame holds only {} distinct colour(s) ({}), so the GPU back buffer did not \
                 contain the page; {} was kept",
                stats.distinct_colors,
                stats.uniform_colour,
                capture_path.display()
            ));
        }

        Ok((
            Capture {
                theme: name,
                path: capture_path.clone(),
                width: image.width,
                height: image.height,
                bytes: png.len() as u64,
                distinct_colors: stats.distinct_colors,
                mean_luminance: stats.mean_luminance,
                window_visible,
            },
            image,
        ))
    })?;
    Ok((capture, image))
}

#[cfg(target_os = "windows")]
fn set_theme(wx: &mut AsyncWindowContext, choice: ThemeChoice) -> Result<(), String> {
    wx.update_root(move |view, _window, cx| {
        let view = view
            .downcast::<ShelluneView>()
            .map_err(|_| "the capture window is not a Shellune view".to_owned())?;
        view.update(cx, |view, cx| view.set_capture_theme(choice, cx));
        Ok(())
    })
    .map_err(|error| format!("the capture window went away: {error:#}"))?
}

/// Asks the window to paint until it has drawn `target` frames.
///
/// Each pass marks the window dirty and posts the paint message the platform posts when a
/// visible window needs a new frame. The message is handled after the closure returns, so
/// gpui never draws while the caller still holds its state.
#[cfg(target_os = "windows")]
async fn settle(
    wx: &mut AsyncWindowContext,
    state: &Arc<Mutex<ProbeState>>,
    target: u32,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let drawn = frames_drawn(state);
        if drawn >= target {
            return Ok(());
        }
        if started.elapsed() > SETTLE_TIMEOUT {
            return Err(format!(
                "the hidden window drew {drawn} frame(s) in {} ms, short of the {target} needed; \
                 a window that is never shown is not being painted by the system",
                SETTLE_TIMEOUT.as_millis()
            ));
        }
        on_window(wx, |window, _cx| {
            window.refresh();
            windows::post_paint(hwnd(window)?);
            Ok(())
        })?;
        wx.background_executor().timer(SETTLE_POLL).await;
    }
}

/// Runs a closure against the window, turning a closed window into a readable reason.
#[cfg(target_os = "windows")]
fn on_window<T>(
    wx: &mut AsyncWindowContext,
    work: impl FnOnce(&mut Window, &mut App) -> Result<T, String>,
) -> Result<T, String> {
    match wx.update(work) {
        Ok(result) => result,
        Err(error) => Err(format!("the capture window went away: {error:#}")),
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

/// Ends the run when the probe could not even start.
#[cfg(target_os = "windows")]
fn fail(cx: &mut App, options: &Options, state: &Arc<Mutex<ProbeState>>, message: String) {
    let report = Report {
        status: "failed",
        error: Some(message.clone()),
        logical_width: options.width,
        logical_height: options.height,
        frames: frames_drawn(state),
        ..Report::default()
    };
    write_report(&options.out, &report, options);
    eprintln!("shellune ui-capture: failed: {message}");
    if let Ok(mut state) = state.lock() {
        state.exit_code = Some(1);
    }
    cx.quit();
}

fn write_report(out: &Path, report: &Report, options: &Options) {
    let path = out.join("capture-probe.json");
    let json = report.to_json(options);
    match serde_json::to_vec_pretty(&json) {
        Ok(bytes) => {
            if let Err(error) = std::fs::write(&path, bytes) {
                eprintln!(
                    "shellune ui-capture: could not write {}: {error}",
                    path.display()
                );
            }
        }
        Err(error) => eprintln!("shellune ui-capture: could not encode the report: {error}"),
    }
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
    let pixels = (image.rgb.len() / 3) as f64;
    Ok(Stats {
        distinct_colors: seen.len(),
        mean_luminance: sum / pixels,
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

/// The share of pixels that differ between two frames of the same size.
#[cfg(target_os = "windows")]
fn pixel_difference(left: &windows::Image, right: &windows::Image) -> Result<f64, String> {
    if left.width != right.width || left.height != right.height {
        return Err(format!(
            "the two frames are different sizes ({}x{} and {}x{}), so they cannot be compared",
            left.width, left.height, right.width, right.height
        ));
    }
    let (lefts, _) = left.rgb.as_chunks::<3>();
    let (rights, _) = right.rgb.as_chunks::<3>();
    let mut different = 0usize;
    for (left, right) in lefts.iter().zip(rights.iter()) {
        if left
            .iter()
            .zip(right)
            .any(|(left, right)| left.abs_diff(*right) > 8)
        {
            different += 1;
        }
    }
    Ok(different as f64 / lefts.len() as f64)
}

/// A PNG of the frame, encoded here so the probe needs no image crate.
///
/// The deflate stream uses stored blocks: the probe's job is proving a capture path, not
/// compressing an image, and a stored stream cannot disagree with the pixels.
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
/// The decision logic the probe runs on the pixels it read.
///
/// These run without a window: what is left to test on a machine that cannot capture is
/// whether a blank frame is rejected, whether two palettes are told apart, and whether
/// the PNG the probe writes is a PNG.
#[cfg(all(test, target_os = "windows"))]
mod tests {
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
        // A page has text, borders and two surfaces: the floor exists to separate that
        // from the one colour a window with nothing to render answers with.
        let drawn = image(16, 16, |x, y| [x as u8, y as u8, 0x40]);
        let stats = analyse(&drawn).expect("a frame with pixels is analysed");
        assert_eq!(stats.distinct_colors, 256);
        assert!(stats.distinct_colors >= DISTINCT_COLOR_FLOOR);
    }

    #[test]
    fn frames_of_different_palettes_are_counted_apart() {
        let dark = image(8, 8, |_, _| [0x1a, 0x1b, 0x1d]);
        let light = image(8, 8, |_, _| [0xf5, 0xf4, 0xf1]);
        let ratio = pixel_difference(&dark, &light).expect("same size");
        assert!((ratio - 1.0).abs() < f64::EPSILON);
        assert!(ratio >= THEME_PIXEL_FLOOR);
        assert!(pixel_difference(&dark, &dark).is_ok_and(|same| same == 0.0));
    }

    #[test]
    fn frames_of_different_sizes_are_refused_rather_than_compared() {
        let small = image(2, 2, |_, _| [0; 3]);
        let large = image(3, 2, |_, _| [0; 3]);
        assert!(pixel_difference(&small, &large).is_err());
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
                0, 0, 0, 0x7f, 1, 0, 0x7f, 2, 0,
                0x7f, // first row: filter byte, then x,y,0x7f
                0, 0, 1, 0x7f, 1, 1, 0x7f, 2, 1, 0x7f, // second row: y is 1 now
            ]
        );
        let stored = u32::from_be_bytes(idat[at..at + 4].try_into().expect("adler"));
        assert_eq!(stored, adler32(&raw));
    }
}
