//! The window's start: the brand while the stores are read, then the page (R7).
//!
//! A launch opens the window before anything is read. The first frame is the brand:
//! the logo, the product name and the tagline settling into place on the window's
//! floor. The hosts, the groups, the trust file, the tunnels and the saved layout are
//! read on the background executor meanwhile. Once they are read the window adopts
//! them on the main thread, starts the work that needed them (sync, the update check,
//! the interrupted batch run), and the brand fades out over the page it became.
//!
//! The brand is shown for one entrance at least and never held longer than the reads
//! take. With reduced motion there is no brand at all: the window shows its bare floor
//! until the page is ready, and the page appears at once. A read that fails ends the
//! start the same way; the page then shows the failure card it always showed.
//!
//! The caption is live the whole time: the top row drags the window, and on Windows
//! the minimise, maximise and close buttons answer, so a slow disk never leaves a
//! window that cannot be moved or closed.

use gpui::prelude::*;
use gpui::{Animation, AnimationExt, AnyElement, Context, Div, SharedString, Window, div};

use shellune_core::{ForwardBook, GroupBook, WorkspaceSnapshot};
use shellune_storage::{
    ForwardStore, GroupService, HostBook, HostStore, HostTrust, TrustStore, WorkspaceStore,
};

use super::{ShelluneView, brand_mark};
use crate::app::copy::Locale;
use crate::app::product;
use crate::app::theme::{self, Enter, Metrics, Motion, palette, spacing};
use crate::app::workspace::Workspace;

/// Where the window is in its start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Startup {
    /// The stores are being read; the window shows the brand, or its bare floor with
    /// reduced motion.
    Splash,
    /// Everything is read and adopted; the window shows its pages.
    Ready,
}

/// What the background read of a launch found, one result per store.
///
/// Each failure is kept as the words the storage layer gave, which is all the window
/// ever showed of it: the shell reads the same results it would have read inline.
pub(super) struct StartupRead {
    hosts: Result<HostBook, String>,
    groups: Result<GroupBook, String>,
    trust: Result<HostTrust, String>,
    forwards: Result<ForwardBook, String>,
    layout: Result<WorkspaceSnapshot, String>,
}

/// The stores a launch reads, cloned off the window so the read can leave the main
/// thread.
struct StartupStores {
    hosts: HostStore,
    groups: GroupService,
    trust: TrustStore,
    forwards: ForwardStore,
    layout: WorkspaceStore,
}

impl StartupStores {
    /// Reads every store, in the order the inline start reads them.
    fn read(self) -> StartupRead {
        fn words<T, E: std::fmt::Display>(read: Result<T, E>) -> Result<T, String> {
            read.map_err(|error| error.to_string())
        }
        StartupRead {
            hosts: words(self.hosts.load()),
            groups: words(self.groups.groups().load()),
            trust: words(self.trust.load()),
            forwards: words(self.forwards.load()),
            layout: words(self.layout.load()),
        }
    }
}

impl ShelluneView {
    /// Builds the shell the way a launch does: the window opens on the brand at once,
    /// and the stores are read behind it (R7).
    ///
    /// Only the preferences are read before the first frame: they say which palette,
    /// language, text size and motion that frame is drawn with, and which keys are
    /// bound from the start. They are one small file.
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
    /// start a launch goes through (R7).
    #[cfg(test)]
    pub fn launching_for_test(
        cx: &mut Context<Self>,
        workspace: Workspace,
        store: HostStore,
    ) -> Self {
        let trust = TrustStore::at(store.root().to_path_buf());
        Self::launching_with(cx, workspace, store, trust, super::default_secrets())
    }

    fn launching_with(
        cx: &mut Context<Self>,
        workspace: Workspace,
        store: HostStore,
        trust: TrustStore,
        secrets: std::sync::Arc<dyn shellune_storage::SecretStore>,
    ) -> Self {
        let mut view = Self::assemble(cx, workspace, store, trust, secrets, true);
        view.restore_preferences(cx);
        view.relabel_fields(cx);
        // Whether the system asks for less motion decides whether there is a brand to
        // show at all, so it is asked before the read can finish.
        view.refresh_system_motion(cx);
        view.startup = Startup::Splash;
        view.read_stores_in_background(cx);
        view
    }

    /// Whether the window is still showing its start rather than its pages.
    #[must_use]
    #[cfg_attr(not(any(test, feature = "ui-capture")), allow(dead_code))]
    pub fn starting(&self) -> bool {
        self.startup == Startup::Splash
    }

    /// Whether the brand is still fading out over the page it became.
    #[cfg(test)]
    pub fn splash_fading(&self) -> bool {
        self.splash_fading
    }

    /// Reads every store on the background executor and adopts the result on the main
    /// thread, after the brand's entrance when there is one (R7).
    fn read_stores_in_background(&mut self, cx: &mut Context<Self>) {
        let root = self.store.root().to_path_buf();
        let stores = StartupStores {
            hosts: self.store.clone(),
            groups: self.group_service.clone(),
            trust: self.trust.clone(),
            forwards: ForwardStore::at(root.clone()),
            layout: WorkspaceStore::at(root),
        };
        let read = cx.background_executor().spawn(async move { stores.read() });
        // The entrance is timed from now, the moment before the first frame, so a read
        // that takes longer than the entrance adds nothing to it.
        let entrance = cx.background_executor().timer(Motion::SLOW);
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let animate = this
                .read_with(cx, |view, _| view.effective_motion().allows_animation())
                .unwrap_or(false);
            if animate {
                entrance.await;
            }
            let _ = this.update(cx, |view, cx| view.finish_startup(read, cx));
        })
        .detach();
    }

    /// Adopts what the background read found, starts the work that waited for it, and
    /// shows the page (R7).
    ///
    /// The order is the inline start's: the hosts before the layout, so a restored pane
    /// that names a deleted record is marked; sync, the update check and the batch
    /// recovery only once everything is in.
    pub(super) fn finish_startup(&mut self, read: StartupRead, cx: &mut Context<Self>) {
        if self.startup == Startup::Ready {
            return;
        }
        self.restore_hosts_from(Some(read.hosts), cx);
        self.adopt_groups(read.groups);
        self.adopt_trust(read.trust);
        self.adopt_forwards(read.forwards);
        self.adopt_workspace_layout(read.layout);
        self.relabel_fields(cx);
        self.begin_work(cx);
        self.startup = Startup::Ready;
        self.splash_fading = self.effective_motion().allows_animation();
        if self.splash_fading {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Motion::BASE).await;
                let _ = this.update(cx, |view, cx| {
                    view.splash_fading = false;
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Whether the brand is drawn: always with full motion, and in the capture's still
    /// splash although a capture draws with reduced motion.
    fn splash_brand_shown(&self) -> bool {
        #[cfg(feature = "ui-capture")]
        if self.splash_held {
            return true;
        }
        theme::motion_allowed()
    }

    /// Puts the window back on its start, held there for a still shot (R7).
    ///
    /// Nothing is read again and nothing is started: the window simply draws the
    /// brand in its final place until the capture has taken its frame.
    #[cfg(feature = "ui-capture")]
    pub fn capture_hold_splash(&mut self, cx: &mut Context<Self>) {
        self.startup = Startup::Splash;
        self.splash_held = true;
        cx.notify();
    }

    /// The whole window while the stores are read: the live caption and, with full
    /// motion, the brand settling into the middle of the floor (R7).
    pub(super) fn startup_splash(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let metrics = self.metrics();
        let brand = self.splash_brand_shown().then(|| {
            theme::enter(
                splash_brand(self.workspace.locale(), metrics),
                "startup.splash.brand",
                Enter::Settle,
            )
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
            .child(self.startup_caption(window, cx))
            .child(splash_body(brand))
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

    /// The brand fading out over the page it became, while it does (R7).
    ///
    /// It is a picture only: it takes no pointer, so the page under it answers from
    /// the first frame it is drawn.
    pub(super) fn startup_crossfade(&self) -> Option<AnyElement> {
        if !self.splash_fading || !theme::motion_allowed() {
            return None;
        }
        let metrics = self.metrics();
        let layer = div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::swatch(palette::CANVAS))
            .debug_selector(|| "startup.crossfade".to_owned())
            // The caption's room, so the brand stays exactly where the splash drew it.
            .child(div().flex_none().h(metrics.header_height()))
            .child(splash_body(Some(
                splash_brand(self.workspace.locale(), metrics).into_any_element(),
            )));
        Some(
            layer
                .with_animation(
                    "startup.crossfade",
                    Animation::new(Motion::BASE).with_easing(theme::ease_out()),
                    |layer, delta| layer.opacity(1.0 - delta),
                )
                .into_any_element(),
        )
    }
}

/// The floor under the caption, with the brand centred in it. Centring on the floor
/// rather than on the whole window keeps the brand clear of the caption at 200% text
/// in the smallest window, where it takes most of the height (R9).
fn splash_body(brand: Option<AnyElement>) -> Div {
    div()
        .flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .children(brand)
}

/// The logo, the product name and the tagline of the language on screen.
fn splash_brand(locale: Locale, metrics: Metrics) -> Div {
    let tagline = match locale {
        Locale::English => product::TAGLINE_EN,
        Locale::Chinese => product::TAGLINE_ZH,
    };
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(metrics.scale(spacing::TIGHT))
        // Never wider than the window: at 200% text in the smallest window the
        // tagline wraps instead of running past the edges (R9).
        .max_w_full()
        .px(metrics.scale(spacing::LOOSE))
        .debug_selector(|| "startup.splash.brand".to_owned())
        .child(brand_mark(metrics.scale(72.0), metrics))
        .child(
            div()
                .mt(metrics.scale(spacing::HAIRLINE))
                .text_size(metrics.hero_size())
                .font_weight(metrics.title_weight())
                .text_color(theme::swatch(palette::TEXT_PRIMARY))
                .debug_selector(|| "startup.splash.name".to_owned())
                .child(SharedString::from(product::NAME)),
        )
        .child(
            div()
                .text_size(metrics.body_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .max_w_full()
                .text_center()
                .debug_selector(|| "startup.splash.tagline".to_owned())
                .child(SharedString::from(tagline)),
        )
}
