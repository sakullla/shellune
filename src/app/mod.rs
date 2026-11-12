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
//! * [`keymap`] — the action catalogue and the bindings in force (R5);
//! * [`workspace`] — the shell state and its honest section statuses;
//! * [`snippets`] — the saved shortcut commands and the manager's own state (R6);
//! * [`components`] — reusable view pieces;
//! * [`view`] — the composition the user actually sees.

#[cfg(feature = "ui-capture")]
pub mod capture;
pub mod components;
pub mod copy;
pub mod files;
pub mod forwarding;
pub mod glyphs;
pub mod hosts;
pub mod icons;
pub mod keymap;
pub mod layout;
pub mod nav;
pub mod palette;
pub mod product;
pub mod remote;
pub mod snippets;
pub mod terminal;
pub mod text_field;
pub mod theme;
pub mod transfers;
mod view;
#[cfg(test)]
pub use view::custom_titlebar as titlebar_for_test;
pub mod workspace;

#[cfg(test)]
mod tests;

pub use keymap::TERMINAL_KEY_CONTEXT;
pub use view::ShelluneView;

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};

use crate::app::keymap::Keymap;
use crate::app::nav::Platform;

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
            GoToForwards,
            GoToSettings,
            NextDestination,
            PreviousDestination,
            CycleTextSize,
            ToggleLanguage,
            DismissNotice,
            NewLocalShell,
            OpenPalette,
            CloseActiveTab,
            NextTab,
            PreviousTab,
            SelectTab1,
            SelectTab2,
            SelectTab3,
            SelectTab4,
            SelectTab5,
            SelectTab6,
            SelectTab7,
            SelectTab8,
            SelectTab9,
            ToggleSidebar,
            SplitPaneRight,
            SplitPaneDown,
            ZoomPane,
            FocusPaneLeft,
            FocusPaneRight,
            FocusPaneUp,
            FocusPaneDown,
            ResizePaneLeft,
            ResizePaneRight,
            ResizePaneUp,
            ResizePaneDown,
            MovePaneLeft,
            MovePaneRight,
            MovePaneUp,
            MovePaneDown,
            MoveTabLeft,
            MoveTabRight,
            FilesUpload,
            FilesDownload,
            TerminalSearch,
            TerminalCopy,
            TerminalPaste
        ]
    );
}

/// Shortcuts for one platform, as a fresh set of defaults.
///
/// The catalogue in [`keymap`] is the only place a shortcut is written down; this
/// function just asks it for the defaults, which is what a first start and a damaged
/// preferences file both bind. A user's changes come from [`Keymap::bindings`] after
/// [`Keymap::resolve`].
#[must_use]
pub fn key_bindings(platform: Platform) -> Vec<KeyBinding> {
    Keymap::defaults(platform).bindings()
}

/// Replaces every binding of the application with the ones a map holds.
///
/// GPUI has no per-binding removal, so the whole key map is rebuilt: unbound actions
/// then really are unbound and their combinations fall through to the terminal
/// program, which is what "hand the key back" means.
pub fn apply_keymap(cx: &mut App, keymap: &Keymap) {
    cx.clear_key_bindings();
    cx.bind_keys(keymap.bindings());
}

/// Number of tabs that have a jump shortcut.
pub const TAB_JUMP_COUNT: usize = 9;

/// Minimum window size the layout is designed for.
pub const MIN_WINDOW_SIZE: (f32, f32) = (720.0, 480.0);

/// Default window size.
pub const DEFAULT_WINDOW_SIZE: (f32, f32) = (1120.0, 720.0);

/// Starts the Shellune application and opens the main window.
pub fn run() {
    Application::new()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
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
        // Windows and macOS draw no title bar of their own: the top row of the window
        // is the caption (see `view::titlebar`). Linux keeps the system's.
        titlebar: Some(TitlebarOptions {
            title: Some(product::NAME.into()),
            appears_transparent: view::custom_titlebar(Platform::current()),
            traffic_light_position: (Platform::current() == Platform::MacOs).then(|| {
                let (x, y) = view::TRAFFIC_LIGHT_POSITION;
                gpui::point(px(x), px(y))
            }),
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
