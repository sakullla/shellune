//! The window's start: the brand and a progress bar that follows the real work, then
//! the page (R3).
//!
//! A launch opens the window before most of the data is read. The start runs four
//! stages, one after another, and each one reports to the main thread as it ends:
//!
//! 1. **settings and theme** - `preferences.json`, read before the first frame because
//!    it says which palette, language, text size, motion and keys that frame has;
//! 2. **hosts and known servers** - the host book and the trust file, on the
//!    background executor;
//! 3. **forwards, groups and workspace** - the tunnels, the group tree and
//!    `workspace.json`, on the background executor;
//! 4. **interface** - the terminal's monospace face resolved and measured once, and
//!    the first frame of the start drawn.
//!
//! The first frame is the brand - the logo and the product name - with a thin bar under
//! it and the running stage in a caption line. The bar's target is `i/4` once stage `i`
//! has ended, and the bar eases to it over [`Motion::BASE`]; the caption fades in with
//! each new stage. The brand has one entrance of [`BRAND_ENTRANCE`] (a fade and a
//! slight zoom) and is never held longer than the stages take. When both are over,
//! the start fades out over [`SPLASH_EXIT`] and the page fades in over
//! [`PAGE_ENTRANCE`], rising [`PAGE_TRAVEL`] into place - on the hosts home the sidebar
//! first and the list [`PAGE_STAGGER`] after it. With reduced motion only the
//! fades are left: no zoom, no rise, and the bar steps instead of easing.
//!
//! A stage that fails stops the start where it is: the page says which stage and why,
//! with "Continue" - the page opens with what could be read, the way a failed read
//! always opened it - and "Open data folder". After a Continue the remaining stages
//! run to the end without stopping again; what they could not read is shown where it
//! belongs, as before.
//!
//! The caption is live the whole time: the top row drags the window, and on Windows
//! the minimise, maximise and close buttons answer, so a slow disk never leaves a
//! window that cannot be moved or closed.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::task::Poll;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Div, KeyDownEvent, SharedString, Task, Window,
    div,
};

use shellune_core::{ForwardBook, GroupBook, WorkspaceSnapshot};
use shellune_storage::{
    ForwardStore, GroupService, HostBook, HostStore, HostTrust, PreferencesProblem,
    PreferencesStore, TrustStore, WorkspaceStore,
};

use super::ShelluneView;
use crate::app::components::{self, Variant};
use crate::app::copy::{self, StartupStrings};
use crate::app::icons;
use crate::app::product;
use crate::app::theme::{self, Metrics, Motion, Tone, palette, spacing};
use crate::app::workspace::Workspace;

/// How long the start waits for the system's reduce-motion answer where it comes from
/// a helper program (Linux), before it decides on its own how the brand enters (R3).
/// The window shows its bare floor meanwhile.
pub const SYSTEM_MOTION_WAIT: Duration = Duration::from_millis(400);

/// The brand's one entrance: the shortest the start is ever shown (R3).
pub const BRAND_ENTRANCE: Duration = Duration::from_millis(250);

/// How long the start takes to fade out over the page it became.
pub const SPLASH_EXIT: Duration = Motion::BASE;

/// How long the page takes to fade in (and, with full motion, rise) under it.
pub const PAGE_ENTRANCE: Duration = Motion::SLOW;

/// How much later the hosts list starts to arrive than the sidebar beside it.
pub const PAGE_STAGGER: Duration = Duration::from_millis(40);

/// How far the page rises into place with full motion, in logical pixels.
pub const PAGE_TRAVEL: f32 = spacing::TIGHT;

/// The size the logo zooms from, as a share of its own, with full motion.
pub const BRAND_ZOOM_FROM: f32 = 0.96;

/// The logo's edge on the start, in logical pixels before zoom.
pub const BRAND_MARK: f32 = 64.0;

/// The logo's edge while a failed stage holds the start, in logical pixels before
/// zoom: smaller, so at 200% text in the smallest window the stage and its reason
/// stay on the first screen instead of scrolling below the brand.
pub const BRAND_MARK_STOPPED: f32 = 40.0;

/// The widest the progress bar is, in logical pixels before zoom.
const BAR_WIDTH: f32 = 200.0;

/// The widest the words of a failed stage run, in logical pixels before zoom.
const FAILURE_WIDTH: f32 = components::STATUS_CARD_WIDTH;

/// Where the window is in its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Startup {
    /// The stages are running, or one of them stopped the start; the window shows the
    /// start page.
    Splash,
    /// Everything is read and adopted; the window shows its pages.
    Ready,
}

/// One stage of the start, in the order they run (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupStage {
    /// `preferences.json`: palette, language, text size, motion and keys.
    Settings,
    /// The host book and the trust file.
    Hosts,
    /// The tunnels, the group tree and the saved layout.
    Workspace,
    /// The monospace face measured and the first frame drawn.
    Interface,
}

impl StartupStage {
    /// The four stages, in the order they run.
    pub const ALL: [Self; 4] = [
        Self::Settings,
        Self::Hosts,
        Self::Workspace,
        Self::Interface,
    ];

    /// The stage's place, from 1.
    #[must_use]
    pub fn number(self) -> usize {
        Self::ALL
            .iter()
            .position(|stage| *stage == self)
            .map_or(0, |index| index + 1)
    }

    /// The caption under the bar while the stage runs.
    fn caption(self, strings: &StartupStrings) -> &'static str {
        match self {
            Self::Settings => strings.stage_settings,
            Self::Hosts => strings.stage_hosts,
            Self::Workspace => strings.stage_workspace,
            Self::Interface => strings.stage_interface,
        }
    }

    /// The title of the stop the stage makes when it fails.
    fn failure_title(self, strings: &StartupStrings) -> &'static str {
        match self {
            Self::Settings => strings.failed_settings,
            Self::Hosts => strings.failed_hosts,
            Self::Workspace => strings.failed_workspace,
            Self::Interface => strings.failed_interface,
        }
    }

    /// The stage's name in the timing log.
    fn log_name(self) -> &'static str {
        match self {
            Self::Settings => "settings and theme",
            Self::Hosts => "hosts and known servers",
            Self::Workspace => "forwards, groups and workspace",
            Self::Interface => "fonts and first frame",
        }
    }
}

/// How the start moves under one motion setting (R3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Choreography {
    /// The share of its size the logo zooms from; 1 is no zoom.
    pub brand_zoom_from: f32,
    /// How far the page rises into place, in logical pixels; 0 is a fade only.
    pub page_travel: f32,
    /// Whether the bar eases to a new value rather than stepping to it.
    pub bar_eases: bool,
}

impl Choreography {
    /// Full motion zooms, rises and eases; reduced motion keeps only the fades.
    #[must_use]
    pub fn for_motion(motion: Motion) -> Self {
        if motion.allows_animation() {
            Self {
                brand_zoom_from: BRAND_ZOOM_FROM,
                page_travel: PAGE_TRAVEL,
                bar_eases: true,
            }
        } else {
            Self {
                brand_zoom_from: 1.0,
                page_travel: 0.0,
                bar_eases: false,
            }
        }
    }
}

/// One stage as it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageReport {
    /// Which stage.
    pub stage: StartupStage,
    /// How long it took, from its start to its report on the main thread.
    pub took: Duration,
    /// Why it failed, in the words the storage layer gave; `None` when it did not.
    pub failure: Option<String>,
}

/// What stage 2 read.
struct HostsRead {
    hosts: Result<HostBook, String>,
    trust: Result<HostTrust, String>,
}

/// What stage 3 read.
struct WorkspaceRead {
    forwards: Result<ForwardBook, String>,
    groups: Result<GroupBook, String>,
    layout: Result<WorkspaceSnapshot, String>,
}

/// The progress of a start: the stages that reported, what they read, and what the
/// start still waits for (R3).
#[derive(Default)]
pub(super) struct StartupRun {
    /// When the launch began, for the total in the timing log.
    began: Option<Instant>,
    /// When the running stage began.
    stage_began: Option<Instant>,
    /// The stages that ended, in order.
    reports: Vec<StageReport>,
    /// Whether the user chose to go on past a failed stage: later failures no longer
    /// stop the start.
    continued: bool,
    hosts: Option<HostsRead>,
    workspace: Option<WorkspaceRead>,
    /// Stage 4: the monospace face is resolved and measured.
    fonts_warm: bool,
    /// Stage 4: the start has drawn a frame.
    frame_drawn: bool,
    /// The brand has had its entrance.
    entrance_over: bool,
    /// Where the bar's fill is going and where it set off from.
    bar: BarMotion,
}

/// The bar's fill on its way to the share of the stages behind the start: it eases from
/// `from` to `to` over [`Motion::BASE`] from `since` (ADR-4).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct BarMotion {
    /// Where the fill set off: where it was drawn when its target last changed.
    pub(super) from: f32,
    /// Where it is going.
    pub(super) to: f32,
    /// When it set off; `None` before the first stage reported.
    since: Option<Instant>,
}

impl BarMotion {
    /// Where the fill is drawn at `now`, on the ease-out-cubic curve the bar uses.
    pub(super) fn at(&self, now: Instant) -> f32 {
        let Some(since) = self.since else {
            return self.to;
        };
        let t = (now.saturating_duration_since(since).as_secs_f32() / Motion::BASE.as_secs_f32())
            .clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from + (self.to - self.from) * eased
    }

    /// Sends the fill to `to`, setting off from where it is drawn at `now`; a target it
    /// already has changes nothing.
    pub(super) fn retarget(&mut self, to: f32, now: Instant) {
        if (to - self.to).abs() < f32::EPSILON {
            return;
        }
        *self = Self {
            from: self.at(now),
            to,
            since: Some(now),
        };
    }
}

impl StartupRun {
    fn began_now() -> Self {
        let now = Instant::now();
        Self {
            began: Some(now),
            stage_began: Some(now),
            ..Self::default()
        }
    }

    /// The stage that stopped the start and is waiting for the user, with its reason.
    fn stop(&self) -> Option<(StartupStage, &str)> {
        if self.continued {
            return None;
        }
        self.reports
            .last()
            .and_then(|report| Some((report.stage, report.failure.as_deref()?)))
    }

    /// How many stages are behind the start: the failed stage that stops it is not.
    fn done(&self) -> usize {
        self.reports.len() - usize::from(self.stop().is_some())
    }

    /// The stage that runs next, or that stopped the start.
    fn current(&self) -> Option<StartupStage> {
        StartupStage::ALL.get(self.done()).copied()
    }

    /// The bar's target: the share of the stages behind the start.
    fn target(&self) -> f32 {
        stage_share(self.done())
    }
}

/// The share of the bar `done` stages fill.
fn stage_share(done: usize) -> f32 {
    let done = u8::try_from(done.min(StartupStage::ALL.len())).unwrap_or(0);
    let all = u8::try_from(StartupStage::ALL.len()).unwrap_or(1);
    f32::from(done) / f32::from(all)
}

/// Whether the timing of each stage is written to the log: always in a development
/// build, and in any build started with `SHELLUNE_STARTUP_LOG` set. Tests keep quiet.
fn logs_timing() -> bool {
    !cfg!(test) && (cfg!(debug_assertions) || std::env::var_os("SHELLUNE_STARTUP_LOG").is_some())
}

/// Milliseconds with one decimal, for the log.
fn millis(duration: Duration) -> String {
    format!("{:.1} ms", duration.as_secs_f64() * 1000.0)
}

/// The `file://` address of a folder, for the system's file manager.
///
/// Separators become `/`, a drive letter gets the leading `/` of a file URL, and every
/// byte outside the unreserved set is percent-encoded, so a folder under a user name
/// with spaces or non-ASCII letters still opens.
#[must_use]
pub fn folder_url(path: &Path) -> String {
    use std::fmt::Write as _;
    let text = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            url.push(char::from(byte));
        } else {
            let _ = write!(url, "%{byte:02X}");
        }
    }
    url
}

/// The stores a launch reads behind the start, cloned off the window so the reads can
/// leave the main thread.
#[derive(Clone)]
struct StartupStores {
    hosts: HostStore,
    groups: GroupService,
    trust: TrustStore,
    forwards: ForwardStore,
    layout: WorkspaceStore,
}

fn words<T, E: std::fmt::Display>(read: Result<T, E>) -> Result<T, String> {
    read.map_err(|error| error.to_string())
}

impl StartupStores {
    /// Stage 2: the host book, then the trust file.
    fn read_hosts(&self) -> HostsRead {
        HostsRead {
            hosts: words(self.hosts.load()),
            trust: words(self.trust.load()),
        }
    }

    /// Stage 3: the tunnels, the group tree, then the saved layout.
    fn read_workspace(&self) -> WorkspaceRead {
        WorkspaceRead {
            forwards: words(self.forwards.load()),
            groups: words(self.groups.groups().load()),
            layout: words(self.layout.load()),
        }
    }
}

/// The first failure among `reads`, if any.
fn first_failure<'a>(reads: impl IntoIterator<Item = Option<&'a String>>) -> Option<String> {
    reads.into_iter().flatten().next().cloned()
}

impl ShelluneView {
    /// Builds the shell the way a launch does: the window opens on the start at once,
    /// and the stages run behind it (R3).
    pub fn launching(cx: &mut Context<Self>) -> Self {
        Self::launching_with(
            cx,
            Workspace::new(),
            super::default_store(),
            super::default_trust(),
            super::default_secrets(),
        )
    }

    /// [`Self::launching`] over a store of the test's own, so a test watches the same
    /// start a launch goes through (R3).
    #[cfg(test)]
    pub fn launching_for_test(
        cx: &mut Context<Self>,
        workspace: Workspace,
        store: HostStore,
    ) -> Self {
        let trust = TrustStore::at(store.root().to_path_buf());
        Self::launching_with(cx, workspace, store, trust, super::default_secrets())
    }

    /// [`Self::launching_for_test`] on a system whose reduce-motion answer comes from a
    /// helper program, the way Linux answers: `answer` is that program (R3).
    #[cfg(test)]
    pub fn launching_with_system_answer_for_test(
        cx: &mut Context<Self>,
        workspace: Workspace,
        store: HostStore,
        answer: Task<bool>,
    ) -> Self {
        let trust = TrustStore::at(store.root().to_path_buf());
        Self::launching_with_answer(
            cx,
            workspace,
            store,
            trust,
            super::default_secrets(),
            Some(answer),
        )
    }

    fn launching_with(
        cx: &mut Context<Self>,
        workspace: Workspace,
        store: HostStore,
        trust: TrustStore,
        secrets: std::sync::Arc<dyn shellune_storage::SecretStore>,
    ) -> Self {
        // Where the answer comes from a helper program it is asked off the main
        // thread, and the start waits for it before it decides how the brand enters.
        let answer = theme::SYSTEM_MOTION_QUERY_BLOCKS.then(|| {
            cx.background_executor()
                .spawn(async { theme::system_reduces_motion() })
        });
        Self::launching_with_answer(cx, workspace, store, trust, secrets, answer)
    }

    fn launching_with_answer(
        cx: &mut Context<Self>,
        workspace: Workspace,
        store: HostStore,
        trust: TrustStore,
        secrets: std::sync::Arc<dyn shellune_storage::SecretStore>,
        answer: Option<Task<bool>>,
    ) -> Self {
        let run = StartupRun::began_now();
        let mut view = Self::assemble(cx, workspace, store, trust, secrets, true);
        view.startup = Startup::Splash;
        view.startup_run = run;
        // Stage 1 runs before the first frame: it says how that frame is drawn.
        view.startup_run.stage_began = Some(Instant::now());
        let settings = view.read_settings_stage(cx);
        view.relabel_fields(cx);
        // Whether the system asks for less motion decides how the brand enters, so it
        // is known before the brand is drawn: asked inline where the system answers
        // from memory, waited for (up to a limit) where it does not.
        if answer.is_some() {
            view.system_motion_pending = true;
        } else {
            view.refresh_system_motion(cx);
        }
        view.time_the_entrance(answer, cx);
        view.report_stage(StartupStage::Settings, settings, cx);
        view
    }

    /// Stage 1: reads `preferences.json` and applies it. A file that is there but is
    /// not a settings document fails the stage; the defaults stay in use, as they
    /// always did.
    fn read_settings_stage(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let store = PreferencesStore::at(self.store.root());
        let (saved, problem) = store.load_reported();
        self.apply_preferences(saved, problem, cx);
        match problem {
            None => Ok(()),
            Some(PreferencesProblem::Unreadable) => {
                let file = store.path().display().to_string();
                Err(copy::fill(
                    copy::startup_for(self.workspace.locale()).settings_unreadable,
                    &[("file", &file)],
                ))
            }
        }
    }

    /// Whether the window is still showing its start rather than its pages.
    #[must_use]
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub fn starting(&self) -> bool {
        self.startup == Startup::Splash
    }

    /// Whether the start is still fading out over the page it became.
    #[cfg(test)]
    pub fn splash_fading(&self) -> bool {
        self.splash_fading
    }

    /// Whether the page is still fading in under the start.
    #[cfg(test)]
    pub fn page_entering(&self) -> bool {
        self.page_entering
    }

    /// Whether the start is still waiting for the system's reduce-motion answer.
    #[cfg(test)]
    pub fn system_motion_pending(&self) -> bool {
        self.system_motion_pending
    }

    /// The stages that ended so far, in order.
    #[cfg(test)]
    pub fn startup_reports(&self) -> Vec<StageReport> {
        self.startup_run.reports.clone()
    }

    /// The bar's target: the share of the stages behind the start.
    #[cfg(test)]
    pub fn startup_target(&self) -> f32 {
        self.startup_run.target()
    }

    /// The share of its size the logo zooms from, for the motion the window draws with.
    #[cfg(test)]
    pub fn brand_zoom_from(&self) -> f32 {
        Choreography::for_motion(self.effective_motion()).brand_zoom_from
    }

    /// The stage that stopped the start, while it waits for the user.
    #[cfg(test)]
    pub fn startup_stop(&self) -> Option<StartupStage> {
        self.startup_run.stop().map(|(stage, _)| stage)
    }

    /// Times the brand's entrance and ends the start once both it and the stages are
    /// over (R3).
    ///
    /// The entrance is timed from the moment the brand can first be drawn - now, before
    /// the first frame, unless the system's answer is awaited - so stages that take
    /// longer than the entrance add nothing to it. With a system `answer` still to
    /// come, the brand waits for it - no longer than [`SYSTEM_MOTION_WAIT`] - and its
    /// entrance is timed from then.
    fn time_the_entrance(&mut self, answer: Option<Task<bool>>, cx: &mut Context<Self>) {
        let early_entrance = answer
            .is_none()
            .then(|| cx.background_executor().timer(BRAND_ENTRANCE));
        cx.spawn(async move |this, cx| {
            let mut entrance = early_entrance;
            if let Some(mut answer) = answer {
                let limit = cx.background_executor().timer(SYSTEM_MOTION_WAIT);
                let reduces = Bounded {
                    answer: &mut answer,
                    limit,
                }
                .await;
                let _ = this.update(cx, |view, cx| {
                    view.system_motion_pending = false;
                    if let Some(reduces) = reduces {
                        view.apply_system_motion(reduces, cx);
                    }
                    cx.notify();
                });
                if reduces.is_none() {
                    // An answer later than the limit still counts for every frame
                    // after it.
                    let late = this.clone();
                    cx.spawn(async move |cx| {
                        let reduces = answer.await;
                        let _ = late.update(cx, |view, cx| view.apply_system_motion(reduces, cx));
                    })
                    .detach();
                }
                entrance = Some(cx.background_executor().timer(BRAND_ENTRANCE));
            }
            if let Some(entrance) = entrance {
                entrance.await;
            }
            let _ = this.update(cx, |view, cx| {
                view.startup_run.entrance_over = true;
                view.end_startup_when_ready(cx);
            });
        })
        .detach();
    }

    /// Notes that `stage` ended, logs how long it took, and runs the next stage - or
    /// stops the start, when the stage failed and the user has not chosen to go on.
    fn report_stage(
        &mut self,
        stage: StartupStage,
        outcome: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let took = self
            .startup_run
            .stage_began
            .map(|began| began.elapsed())
            .unwrap_or_default();
        if logs_timing() {
            let state = match &outcome {
                Ok(()) => String::new(),
                Err(reason) => format!(" (failed: {reason})"),
            };
            eprintln!(
                "shellune start: stage {}/{} {}: {}{state}",
                stage.number(),
                StartupStage::ALL.len(),
                stage.log_name(),
                millis(took),
            );
        }
        self.startup_run.reports.push(StageReport {
            stage,
            took,
            failure: outcome.err(),
        });
        let target = self.startup_run.target();
        self.startup_run.bar.retarget(target, Instant::now());
        cx.notify();
        if self.startup_run.stop().is_none() {
            self.run_next_stage(cx);
        }
    }

    /// Starts the stage after the last one that reported, or ends the start when
    /// there is none left.
    fn run_next_stage(&mut self, cx: &mut Context<Self>) {
        self.startup_run.stage_began = Some(Instant::now());
        let next = StartupStage::ALL
            .get(self.startup_run.reports.len())
            .copied();
        let root = self.store.root().to_path_buf();
        let stores = StartupStores {
            hosts: self.store.clone(),
            groups: self.group_service.clone(),
            trust: self.trust.clone(),
            forwards: ForwardStore::at(root.clone()),
            layout: WorkspaceStore::at(root),
        };
        match next {
            // Stage 1 is read inline before the first frame and always reports first.
            Some(StartupStage::Settings) | None => self.end_startup_when_ready(cx),
            Some(StartupStage::Hosts) => {
                let read = cx
                    .background_executor()
                    .spawn(async move { stores.read_hosts() });
                cx.spawn(async move |this, cx| {
                    let read = read.await;
                    let _ = this.update(cx, |view, cx| {
                        let failure =
                            first_failure([read.hosts.as_ref().err(), read.trust.as_ref().err()]);
                        view.startup_run.hosts = Some(read);
                        view.report_stage(StartupStage::Hosts, failure.map_or(Ok(()), Err), cx);
                    });
                })
                .detach();
            }
            Some(StartupStage::Workspace) => {
                let read = cx
                    .background_executor()
                    .spawn(async move { stores.read_workspace() });
                cx.spawn(async move |this, cx| {
                    let read = read.await;
                    let _ = this.update(cx, |view, cx| {
                        let failure = first_failure([
                            read.forwards.as_ref().err(),
                            read.groups.as_ref().err(),
                            read.layout.as_ref().err(),
                        ]);
                        view.startup_run.workspace = Some(read);
                        view.report_stage(StartupStage::Workspace, failure.map_or(Ok(()), Err), cx);
                    });
                })
                .detach();
            }
            Some(StartupStage::Interface) => {
                // The terminal's face is resolved and measured once off the main
                // thread, so the first terminal does not pay for loading it.
                let text_system = cx.text_system().clone();
                let size = gpui::px(self.workspace.local_font_size());
                let warm = cx.background_executor().spawn(async move {
                    let face = gpui::font(crate::app::terminal::monospace_family());
                    let font = text_system.resolve_font(&face);
                    let _ = text_system.em_advance(font, size);
                });
                cx.spawn(async move |this, cx| {
                    warm.await;
                    let _ = this.update(cx, |view, cx| {
                        view.startup_run.fonts_warm = true;
                        view.report_interface_when_ready(cx);
                    });
                })
                .detach();
            }
        }
    }

    /// Stage 4 ends once the face is measured and the start has drawn a frame.
    fn report_interface_when_ready(&mut self, cx: &mut Context<Self>) {
        let run = &self.startup_run;
        let running = run.reports.len() + 1 == StartupStage::Interface.number();
        if self.startup == Startup::Splash && running && run.fonts_warm && run.frame_drawn {
            self.report_stage(StartupStage::Interface, Ok(()), cx);
        }
    }

    /// Ends the start once every stage reported and the brand had its entrance, unless
    /// a failed stage is waiting for the user.
    fn end_startup_when_ready(&mut self, cx: &mut Context<Self>) {
        let run = &self.startup_run;
        if self.startup == Startup::Splash
            && run.entrance_over
            && run.reports.len() == StartupStage::ALL.len()
            && run.stop().is_none()
        {
            self.finish_startup(cx);
        }
    }

    /// The user chose to go on past a failed stage: the page opens with what could be
    /// read, after the stages that are left (R3).
    pub(super) fn continue_startup(&mut self, cx: &mut Context<Self>) {
        if self.startup_run.stop().is_none() {
            return;
        }
        self.startup_run.continued = true;
        let target = self.startup_run.target();
        self.startup_run.bar.retarget(target, Instant::now());
        cx.notify();
        self.run_next_stage(cx);
    }

    /// Opens the folder the data files are in, in the system's file manager.
    pub(super) fn open_data_folder(&mut self, cx: &mut Context<Self>) {
        cx.open_url(&folder_url(self.store.root()));
    }

    /// Adopts what the stages read, starts the work that waited for it, and shows the
    /// page (R3).
    ///
    /// The order is the inline start's: the hosts before the layout, so a restored pane
    /// that names a deleted record is marked; sync, the update check and the batch
    /// recovery only once everything is in.
    fn finish_startup(&mut self, cx: &mut Context<Self>) {
        if self.startup == Startup::Ready {
            return;
        }
        let (hosts, trust) = match self.startup_run.hosts.take() {
            Some(read) => (Some(read.hosts), Some(read.trust)),
            None => (None, None),
        };
        let (forwards, groups, layout) = match self.startup_run.workspace.take() {
            Some(read) => (Some(read.forwards), Some(read.groups), Some(read.layout)),
            None => (None, None, None),
        };
        if let Some(read) = hosts {
            self.restore_hosts_from(Some(read), cx);
        }
        if let Some(read) = groups {
            self.adopt_groups(read);
        }
        if let Some(read) = trust {
            self.adopt_trust(read);
        }
        if let Some(read) = forwards {
            self.adopt_forwards(read);
        }
        if let Some(read) = layout {
            self.adopt_workspace_layout(read);
        }
        self.relabel_fields(cx);
        self.begin_work(cx);
        self.startup = Startup::Ready;
        if logs_timing()
            && let Some(began) = self.startup_run.began
        {
            eprintln!(
                "shellune start: page shown after {}",
                millis(began.elapsed())
            );
        }
        // The start fades out and the page fades in under it, whatever the motion:
        // reduced motion keeps the fades and drops the rise (R3).
        self.splash_fading = true;
        self.page_entering = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SPLASH_EXIT).await;
            let _ = this.update(cx, |view, cx| {
                view.splash_fading = false;
                cx.notify();
            });
            cx.background_executor()
                .timer((PAGE_ENTRANCE + PAGE_STAGGER).saturating_sub(SPLASH_EXIT))
                .await;
            let _ = this.update(cx, |view, cx| {
                view.page_entering = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Whether the start draws the brand: once the motion it enters with is known, and
    /// always in the capture's still start.
    fn splash_brand_shown(&self) -> bool {
        #[cfg(feature = "ui-capture")]
        if self.splash_held {
            return true;
        }
        !self.system_motion_pending
    }

    /// Whether the start is a still picture: the capture's held start, drawn in its
    /// final place without any entrance.
    fn splash_still(&self) -> bool {
        #[cfg(feature = "ui-capture")]
        if self.splash_held {
            return true;
        }
        false
    }

    /// Draws the design catalogue in place of a page, for the capture (ADR-2).
    #[cfg(feature = "ui-capture")]
    pub fn capture_show_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_held = true;
        cx.notify();
    }

    /// Whether the window draws the design catalogue.
    #[cfg(feature = "ui-capture")]
    pub fn catalog_shown(&self) -> bool {
        self.catalog_held
    }

    /// Puts the window back on its start, held there for a still shot (R3): `done`
    /// stages behind it, and the next one running - or, with a `failure`, stopped on
    /// it with that reason.
    ///
    /// Nothing is read again and nothing is started: the window simply draws the start
    /// in its final place until the capture has taken its frame.
    #[cfg(feature = "ui-capture")]
    pub fn capture_hold_startup(
        &mut self,
        done: usize,
        failure: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let mut run = StartupRun::default();
        let stages = StartupStage::ALL
            .into_iter()
            .take(done + usize::from(failure.is_some()));
        for stage in stages {
            run.reports.push(StageReport {
                stage,
                took: Duration::ZERO,
                failure: None,
            });
        }
        if let (Some(reason), Some(last)) = (failure, run.reports.last_mut()) {
            last.failure = Some(reason.to_owned());
        }
        self.startup_run = run;
        self.startup = Startup::Splash;
        self.splash_held = true;
        cx.notify();
    }

    /// The whole window while the start runs: the live caption and, once the motion it
    /// enters with is known, the brand with the progress of the stages - or the stop a
    /// failed stage made (R3).
    pub(super) fn startup_splash(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        // Stage 4 waits for a frame of the start to be drawn: this is it.
        if !self.startup_run.frame_drawn && !self.splash_still() {
            self.startup_run.frame_drawn = true;
            cx.defer_in(window, |view, _window, cx| {
                view.report_interface_when_ready(cx);
            });
        }
        let metrics = self.metrics();
        let brand = self.splash_brand_shown().then(|| self.splash_column());
        let stopped = self.startup_run.stop().is_some();
        // The stop's two buttons stand under the words that scroll, so they are on the
        // first screen even at 200% text in the smallest window (T9).
        let buttons = (stopped && self.splash_brand_shown()).then(|| {
            let strings = copy::startup_for(self.workspace.locale());
            self.startup_stop_buttons(strings, metrics, cx)
        });
        div()
            .id("startup.splash")
            .debug_selector(|| "startup.splash".to_owned())
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::swatch(palette::CANVAS))
            .text_size(metrics.body_size())
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            .key_context("Shellune")
            .track_focus(&self.root_focus)
            // A stopped start goes on with Enter, the way its main button does.
            .when(stopped, |el| {
                el.on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                    if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                        this.continue_startup(cx);
                        cx.stop_propagation();
                    }
                }))
            })
            .child(self.startup_caption(window, cx))
            .child(splash_body(brand, buttons))
            .into_any_element()
    }

    /// The brand and, under it, the progress of the stages or the stop a failed stage
    /// made.
    fn splash_column(&self) -> AnyElement {
        let metrics = self.metrics();
        let locale = self.workspace.locale();
        let strings = copy::startup_for(locale);
        let still = self.splash_still();
        let choreography = Choreography::for_motion(self.effective_motion());
        let stopped = self.startup_run.stop().is_some();
        let status = match self.startup_run.stop() {
            Some((stage, reason)) => Self::startup_stop_panel(stage, reason, strings, metrics),
            None => self.startup_progress(strings, metrics, still, choreography),
        };
        // A stop is about what failed: the brand steps back so the stage and its
        // reason are on the first screen even at 200% text in the smallest window.
        let (mark, name_size) = if stopped {
            (BRAND_MARK_STOPPED, metrics.title_size())
        } else {
            (BRAND_MARK, metrics.hero_size())
        };
        let column = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            // Never wider than the window: at 200% text in the smallest window the
            // words wrap instead of running past the edges.
            .w_full()
            .max_w_full()
            .px(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "startup.splash.brand".to_owned())
            .child(brand_mark(metrics.scale(mark), still, choreography))
            .child(
                div()
                    .mt(metrics.scale(spacing::HAIRLINE))
                    .text_size(name_size)
                    .font_weight(metrics.title_weight())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .debug_selector(|| "startup.splash.name".to_owned())
                    .child(SharedString::from(product::NAME)),
            )
            .child(status);
        if still {
            column.into_any_element()
        } else {
            theme::arrive(column, "startup.splash.brand", BRAND_ENTRANCE, 0.0)
        }
    }

    /// The bar and the caption of the running stage.
    fn startup_progress(
        &self,
        strings: &StartupStrings,
        metrics: Metrics,
        still: bool,
        choreography: Choreography,
    ) -> AnyElement {
        let run = &self.startup_run;
        let done = run.done();
        let caption = run
            .current()
            .map_or(strings.stage_done, |stage| stage.caption(strings));
        // The fill sets off from where it was drawn when the stage ended, not from the
        // previous target: stages closer than the easing do not make it jump (T9).
        let bar = components::progress_line(
            ("startup.progress", done),
            run.bar.from,
            run.bar.to,
            Motion::BASE,
            still || !choreography.bar_eases,
            metrics,
        );
        let caption = div()
            .max_w_full()
            .text_center()
            .text_size(metrics.caption_size())
            .text_color(theme::swatch(palette::TEXT_SECONDARY))
            .debug_selector(|| "startup.stage".to_owned())
            .child(SharedString::from(caption));
        let caption = if still {
            caption.into_any_element()
        } else {
            theme::arrive(caption, ("startup.stage", done), Motion::FAST, 0.0)
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(metrics.scale(spacing::SNUG))
            .w_full()
            .mt(metrics.scale(spacing::LOOSE))
            .child(
                div()
                    .w(metrics.scale(BAR_WIDTH))
                    .max_w_full()
                    .debug_selector(|| "startup.progress".to_owned())
                    .child(bar),
            )
            .child(caption)
            .into_any_element()
    }

    /// The stop a failed stage makes: which stage, why, and the two ways on (R3).
    fn startup_stop_panel(
        stage: StartupStage,
        reason: &str,
        strings: &StartupStrings,
        metrics: Metrics,
    ) -> AnyElement {
        let width = metrics.scale(FAILURE_WIDTH);
        let line = |text: SharedString, selector: &'static str| {
            components::capped_text(
                width,
                div()
                    .text_center()
                    .debug_selector(move || selector.to_owned())
                    .child(text),
            )
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .w_full()
            .mt(metrics.scale(spacing::LOOSE))
            .debug_selector(|| "startup.failure".to_owned())
            .child(
                line(
                    SharedString::from(stage.failure_title(strings)),
                    "startup.failure.title",
                )
                .text_size(metrics.emphasis_size())
                .font_weight(metrics.title_weight())
                .text_color(Tone::Danger.text()),
            )
            .child(
                line(
                    SharedString::from(reason.to_owned()),
                    "startup.failure.reason",
                )
                .text_size(metrics.body_size())
                .text_color(theme::swatch(palette::TEXT_PRIMARY)),
            )
            .child(
                line(
                    SharedString::from(strings.failed_hint),
                    "startup.failure.hint",
                )
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY)),
            )
            .into_any_element()
    }

    /// The two ways on from a stop: go on, or open the data folder (R3).
    fn startup_stop_buttons(
        &self,
        strings: &StartupStrings,
        metrics: Metrics,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_none()
            .flex_wrap()
            .justify_center()
            .w_full()
            .px(metrics.scale(spacing::LOOSE))
            .gap(metrics.scale(spacing::TIGHT))
            .pt(metrics.scale(spacing::NARROW))
            .pb(metrics.scale(spacing::SNUG))
            .debug_selector(|| "startup.failure.actions".to_owned())
            .child(
                components::button(strings.continue_start, Variant::Primary, metrics)
                    .id("startup.continue")
                    .debug_selector(|| "startup.continue".to_owned())
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        this.continue_startup(cx);
                    })),
            )
            .child(
                components::icon_label_button(
                    icons::Icon::FolderOpen,
                    strings.open_folder,
                    Variant::Secondary,
                    metrics,
                )
                .id("startup.open_folder")
                .debug_selector(|| "startup.open_folder".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.open_data_folder(cx);
                })),
            )
            .into_any_element()
    }

    /// The top row while the window starts: a blank caption that drags the window and,
    /// on Windows, the window's own buttons. On macOS the system draws its buttons over
    /// the same row; on Linux the system's title bar is above it.
    fn startup_caption(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(metrics.header_height())
            .debug_selector(|| "startup.caption".to_owned())
            .child(self.drag_area("startup.drag").flex_1().h_full())
            .children(self.window_controls(window, cx))
    }

    /// The start fading out over the page it became, while it does (R3).
    ///
    /// It is a picture only: it takes no pointer, so the page under it answers from
    /// the first frame it is drawn. It shows the brand and the full bar, as the last
    /// frame of the start did.
    pub(super) fn startup_crossfade(&self) -> Option<AnyElement> {
        if !self.splash_fading {
            return None;
        }
        let metrics = self.metrics();
        let choreography = Choreography::for_motion(self.effective_motion());
        let picture = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .w_full()
            .max_w_full()
            .px(metrics.scale(spacing::LOOSE))
            .child(brand_mark(metrics.scale(BRAND_MARK), true, choreography))
            .child(
                div()
                    .mt(metrics.scale(spacing::HAIRLINE))
                    .text_size(metrics.hero_size())
                    .font_weight(metrics.title_weight())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(product::NAME)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(metrics.scale(spacing::SNUG))
                    .w_full()
                    .mt(metrics.scale(spacing::LOOSE))
                    .child(div().w(metrics.scale(BAR_WIDTH)).max_w_full().child(
                        components::progress_line(
                            "startup.crossfade.progress",
                            1.0,
                            1.0,
                            Motion::BASE,
                            true,
                            metrics,
                        ),
                    ))
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .child(SharedString::from(
                                copy::startup_for(self.workspace.locale()).stage_done,
                            )),
                    ),
            );
        let layer = div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::swatch(palette::CANVAS))
            .debug_selector(|| "startup.crossfade".to_owned())
            // The caption's room, so the brand stays exactly where the start drew it.
            .child(div().flex_none().h(metrics.header_height()))
            .child(splash_body(Some(picture.into_any_element()), None));
        Some(
            layer
                .with_animation(
                    "startup.crossfade",
                    Animation::new(SPLASH_EXIT).with_easing(theme::ease_out()),
                    |layer, delta| layer.opacity(1.0 - delta),
                )
                .into_any_element(),
        )
    }

    /// The page under the fading start: it fades in and, with full motion, rises
    /// [`PAGE_TRAVEL`] into place (R3). Outside that moment it is the page as it is.
    ///
    /// The hosts home arrives in two pieces instead - see [`Self::home_entrance`] - so it
    /// is passed through as it is here.
    pub(super) fn page_entrance(&self, page: impl IntoElement + Styled + 'static) -> AnyElement {
        if !self.page_entering || self.home_arrives_in_pieces() {
            return page.into_any_element();
        }
        let choreography = Choreography::for_motion(self.effective_motion());
        theme::arrive(
            page,
            "startup.page",
            PAGE_ENTRANCE,
            choreography.page_travel,
        )
    }

    /// Whether the page the start hands over to is the hosts home with its sidebar and
    /// its list, which arrive one after the other.
    fn home_arrives_in_pieces(&self) -> bool {
        let destination = self.workspace.destination();
        destination != crate::app::nav::Destination::Settings
            && !self.shows_terminal(destination)
            && !matches!(
                self.workspace.state(crate::app::nav::Destination::Hosts),
                crate::app::workspace::SectionState::Loading
                    | crate::app::workspace::SectionState::Failed
            )
    }

    /// One piece of the hosts home under the fading start: the sidebar first (`order`
    /// 0), then the list [`PAGE_STAGGER`] later (`order` 1). Each fades in and, with
    /// full motion, rises [`PAGE_TRAVEL`] into place over [`PAGE_ENTRANCE`] (R3).
    pub(super) fn home_entrance(
        &self,
        piece: impl IntoElement + Styled + 'static,
        id: &'static str,
        order: u32,
    ) -> AnyElement {
        if !self.page_entering {
            return piece.into_any_element();
        }
        let choreography = Choreography::for_motion(self.effective_motion());
        theme::arrive_after(
            piece,
            id,
            PAGE_STAGGER * order,
            PAGE_ENTRANCE,
            choreography.page_travel,
        )
    }
}

/// Waits for `answer`, but no longer than `limit`: `None` when the limit came first,
/// and `answer` is left to finish on its own.
struct Bounded<'a, A, L> {
    answer: &'a mut A,
    limit: L,
}

impl<A: Future + Unpin, L: Future + Unpin> Future for Bounded<'_, A, L> {
    type Output = Option<A::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if let Poll::Ready(value) = Pin::new(&mut *self.answer).poll(cx) {
            return Poll::Ready(Some(value));
        }
        if Pin::new(&mut self.limit).poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    }
}

/// The floor under the caption, with the start centred in it. Centring on the floor
/// rather than on the whole window keeps the brand clear of the caption at 200% text
/// in the smallest window. A stop that is taller than the floor there scrolls rather
/// than losing its buttons off the edge.
fn splash_body(brand: Option<AnyElement>, buttons: Option<AnyElement>) -> impl IntoElement {
    // The floor centres the start and, under it, a stop's buttons; when the two are
    // taller than the floor the words above the buttons scroll and the buttons stay.
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .justify_center()
        .child(
            div()
                .id("startup.body")
                .flex()
                .flex_col()
                .flex_shrink()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .items_center()
                        .w_full()
                        .children(brand),
                ),
        )
        .children(buttons)
}

/// The logo, zooming from [`Choreography::brand_zoom_from`] of its size into place
/// unless the start is `still` or the motion is reduced.
fn brand_mark(size: gpui::Pixels, still: bool, choreography: Choreography) -> AnyElement {
    let mark = div()
        .flex_none()
        .size(size)
        .child(gpui::img(icons::APP_ICON).size_full());
    let from = choreography.brand_zoom_from;
    let mark = if still || from >= 1.0 {
        mark.debug_selector(|| "startup.splash.mark".to_owned())
            .into_any_element()
    } else {
        mark.debug_selector(|| "startup.splash.mark".to_owned())
            .with_animation(
                "startup.splash.mark",
                Animation::new(BRAND_ENTRANCE).with_easing(theme::ease_out()),
                move |mark, delta| mark.size(size * (from + (1.0 - from) * delta)),
            )
            .into_any_element()
    };
    // The room is the full size from the first frame, so the zoom moves nothing else.
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .child(mark)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_stages_run_in_the_order_the_start_names_them() {
        assert_eq!(
            StartupStage::ALL,
            [
                StartupStage::Settings,
                StartupStage::Hosts,
                StartupStage::Workspace,
                StartupStage::Interface
            ]
        );
        let numbers: Vec<usize> = StartupStage::ALL.iter().map(|s| s.number()).collect();
        assert_eq!(numbers, vec![1, 2, 3, 4]);
    }

    #[test]
    fn the_bar_targets_a_quarter_per_stage_behind_the_start() {
        let shares: Vec<f32> = (0..=5).map(stage_share).collect();
        assert_eq!(shares, vec![0.0, 0.25, 0.5, 0.75, 1.0, 1.0]);
    }

    #[test]
    fn the_bar_sets_off_from_where_it_is_drawn() {
        // T9: two stages 100ms apart - closer than the 200ms easing - send the fill on
        // from where it was drawn, not back to the previous target or ahead of it.
        let start = Instant::now();
        let mut bar = BarMotion::default();
        bar.retarget(stage_share(1), start);
        assert!(bar.from.abs() < f32::EPSILON);
        let halfway = start + Motion::BASE / 2;
        let drawn = bar.at(halfway);
        assert!(drawn > 0.0 && drawn < 0.25, "{drawn}");
        assert!(
            (drawn - 0.25 * 0.875).abs() < 1e-4,
            "ease-out-cubic at half time"
        );
        bar.retarget(stage_share(2), halfway);
        assert!((bar.from - drawn).abs() < 1e-6, "{bar:?}");
        assert!((bar.to - 0.5).abs() < f32::EPSILON);
        assert!(
            (bar.at(halfway) - drawn).abs() < 1e-6,
            "no jump at the hand-over"
        );
        assert!((bar.at(halfway + Motion::BASE) - 0.5).abs() < f32::EPSILON);
        // The same target again changes nothing.
        let kept = bar;
        bar.retarget(stage_share(2), halfway + Motion::BASE);
        assert_eq!(bar, kept);
    }

    #[test]
    fn a_failed_stage_stops_the_start_until_the_user_goes_on() {
        let mut run = StartupRun::default();
        let report = |stage, failure: Option<&str>| StageReport {
            stage,
            took: Duration::ZERO,
            failure: failure.map(str::to_owned),
        };
        run.reports.push(report(StartupStage::Settings, None));
        assert_eq!(run.done(), 1);
        assert_eq!(run.current(), Some(StartupStage::Hosts));
        run.reports
            .push(report(StartupStage::Hosts, Some("hosts.json: denied")));
        assert_eq!(
            run.stop(),
            Some((StartupStage::Hosts, "hosts.json: denied"))
        );
        assert_eq!(run.done(), 1, "the failed stage is not behind the start");
        assert_eq!(run.current(), Some(StartupStage::Hosts));
        assert!((run.target() - 0.25).abs() < f32::EPSILON);

        run.continued = true;
        assert_eq!(run.stop(), None);
        assert_eq!(run.done(), 2, "past a Continue the stage counts as behind");
        run.reports
            .push(report(StartupStage::Workspace, Some("groups.json: denied")));
        assert_eq!(run.stop(), None, "after a Continue nothing stops it again");
    }

    #[test]
    fn reduced_motion_keeps_only_the_fades() {
        let full = Choreography::for_motion(Motion::Full);
        assert!((full.brand_zoom_from - 0.96).abs() < f32::EPSILON);
        assert!((full.page_travel - 8.0).abs() < f32::EPSILON);
        assert!(full.bar_eases);
        let reduced = Choreography::for_motion(Motion::Reduced);
        assert!((reduced.brand_zoom_from - 1.0).abs() < f32::EPSILON);
        assert!(reduced.page_travel.abs() < f32::EPSILON);
        assert!(!reduced.bar_eases);
    }

    #[test]
    fn the_timings_of_the_start_match_the_design() {
        assert_eq!(BRAND_ENTRANCE, Duration::from_millis(250));
        assert_eq!(SPLASH_EXIT, Duration::from_millis(200));
        assert_eq!(PAGE_ENTRANCE, Duration::from_millis(300));
        assert!(PAGE_STAGGER <= Duration::from_millis(40));
    }

    #[test]
    fn the_data_folder_opens_as_a_file_url() {
        assert_eq!(
            folder_url(Path::new("/home/ana/.config/shellune")),
            "file:///home/ana/.config/shellune"
        );
        assert_eq!(
            folder_url(Path::new(r"C:\Users\Ana Li\AppData\Roaming\Shellune")),
            "file:///C:/Users/Ana%20Li/AppData/Roaming/Shellune"
        );
        assert_eq!(
            folder_url(Path::new("/home/李/数据")),
            "file:///home/%E6%9D%8E/%E6%95%B0%E6%8D%AE"
        );
    }
}
