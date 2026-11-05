//! GPUI application shell: product identity, window startup, key bindings and the
//! root view.
//!
//! Everything render-independent (hosts, sessions, transfers, safe errors) lives in
//! `shellune-core`; this module composes it into GPUI state. The pieces are split
//! so that the visual system can change without touching domain logic:
//!
//! * [`product`] — name, pronunciation and the two taglines fixed by R18;
//! * [`copy`] — every rendered string, in English and Chinese;
//! * [`theme`] — palette, spacing, type scale, zoom and contrast arithmetic;
//! * [`nav`] — the destinations and their platform shortcuts;
//! * [`workspace`] — the shell state and its honest section statuses;
//! * [`components`] — reusable view pieces;
//! * [`view`] — the composition the user actually sees.

pub mod components;
pub mod copy;
pub mod nav;
pub mod product;
pub mod terminal;
pub mod text_field;
pub mod theme;
mod view;
pub mod workspace;

#[cfg(test)]
mod tests;

pub use view::ShelluneView;

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};

use crate::app::actions::{
    CycleTextSize, DismissNotice, GoToHosts, GoToLocalShells, GoToSessions, GoToTransfers,
    NextDestination, PreviousDestination, ToggleLanguage,
};
use crate::app::nav::{Destination, Platform};

/// Keyboard actions the shell understands.
///
/// They are actions rather than raw key handlers so that the same intent can be
/// triggered by a shortcut, a menu item or a test without duplicating the logic.
pub mod actions {
    gpui::actions!(
        shellune,
        [
            GoToLocalShells,
            GoToHosts,
            GoToSessions,
            GoToTransfers,
            NextDestination,
            PreviousDestination,
            CycleTextSize,
            ToggleLanguage,
            DismissNotice
        ]
    );
}

/// Key context that scopes the shell's shortcuts.
pub const KEY_CONTEXT: &str = "Shellune";

/// Shortcuts for one platform.
///
/// The navigation digits follow the platform convention: ⌘1-4 on macOS, Ctrl+1-4
/// on Windows and Linux. Everything else stays on the same letter so a user who
/// switches systems keeps one mental model.
pub fn key_bindings(platform: Platform) -> Vec<KeyBinding> {
    let modifier = platform.keystroke_modifier();
    vec![
        KeyBinding::new(
            &Destination::LocalShells.keystroke(platform),
            GoToLocalShells,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &Destination::Hosts.keystroke(platform),
            GoToHosts,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &Destination::Sessions.keystroke(platform),
            GoToSessions,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &Destination::Transfers.keystroke(platform),
            GoToTransfers,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &format!("{modifier}-tab"),
            NextDestination,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &format!("{modifier}-shift-tab"),
            PreviousDestination,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(&format!("{modifier}-="), CycleTextSize, Some(KEY_CONTEXT)),
        KeyBinding::new(&format!("{modifier}-l"), ToggleLanguage, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", DismissNotice, Some(KEY_CONTEXT)),
    ]
}

/// Minimum window size the layout is designed for.
pub const MIN_WINDOW_SIZE: (f32, f32) = (720.0, 480.0);

/// Default window size.
pub const DEFAULT_WINDOW_SIZE: (f32, f32) = (1120.0, 720.0);

/// Starts the Shellune application and opens the main window.
pub fn run() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys(key_bindings(Platform::current()));
        if let Err(error) = open_main_window(cx) {
            eprintln!("shellune: failed to open the main window: {error}");
        }
        cx.activate(true);
    });
}

fn open_main_window(cx: &mut App) -> Result<(), String> {
    let (width, height) = DEFAULT_WINDOW_SIZE;
    let (min_width, min_height) = MIN_WINDOW_SIZE;
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(min_width), px(min_height))),
        titlebar: Some(TitlebarOptions {
            title: Some(product::NAME.into()),
            ..Default::default()
        }),
        app_id: Some(product::APP_ID.to_string()),
        ..Default::default()
    };

    cx.open_window(options, |window, cx| {
        let view = cx.new(ShelluneView::new);
        // Give the shell the keyboard before the user clicks anything, so the
        // navigation shortcuts work in the first frame.
        window.focus(view.read(cx).root_focus());
        view
    })
    .map(|_| ())
    .map_err(|error| format!("{error:#}"))
}
