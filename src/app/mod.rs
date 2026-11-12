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
    App, AppContext, Application, Bounds, Global, KeyBinding, TitlebarOptions, WindowBounds,
    WindowHandle, WindowId, WindowOptions, px, size,
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

/// The main window, for as long as the application keeps it (R13).
///
/// macOS hides the window on the red button instead of destroying it, so the handle has
/// to outlive a frame: a Dock click can ask for the window minutes later. Keeping it here
/// rather than in the view is what lets "one window, never two" be decided before any
/// window is built.
struct MainWindow(Option<WindowHandle<ShelluneView>>);

impl Global for MainWindow {}

/// What tapping the application icon has to do (R13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReopenChoice {
    /// A window this application still owns is alive: bring it back, do not build one.
    Activate(WindowId),
    /// No window survived the close: only now does the one creation path run.
    OpenMain,
}

/// Decides between restoring a surviving window and creating the main one (R13).
///
/// `open` is what the application still has. A remembered handle that is no longer in it
/// is stale - that window was really closed - and must not keep a new window from being
/// opened. Any other answer would either leave the user with no window at all or put a
/// second one beside the window that is merely hidden or minimised.
#[must_use]
pub fn reopen_choice(main: Option<WindowId>, open: &[WindowId]) -> ReopenChoice {
    match main {
        Some(id) if open.contains(&id) => ReopenChoice::Activate(id),
        _ => ReopenChoice::OpenMain,
    }
}

/// Starts the Shellune application and opens the main window.
pub fn run() {
    let application = Application::new().with_assets(icons::Assets);
    // A Dock click or a second launch reaches the application, not a window, so the
    // handler is registered before the first window exists: an early click is not lost.
    application.on_reopen(|cx| {
        if let Err(error) = reopen_main_window(cx) {
            eprintln!("shellune: failed to reopen the main window: {error}");
        }
    });
    application.run(|cx: &mut App| {
        cx.bind_keys(key_bindings(Platform::current()));
        if let Err(error) = open_main_window(cx) {
            eprintln!("shellune: failed to open the main window: {error}");
        }
        cx.activate(true);
    });
}

/// Brings the main window back after the application icon is clicked (R13).
///
/// A window that survived the close - hidden on macOS, or minimised - is activated. Its
/// view, terminals, transfers and forwards were never dropped, so nothing reconnects and
/// no tab is duplicated. Only when no window survives does the shared entry create one,
/// which is also the path a first launch takes.
pub fn reopen_main_window(cx: &mut App) -> Result<(), String> {
    let open: Vec<WindowId> = cx
        .windows()
        .into_iter()
        .map(|handle| handle.window_id())
        .collect();
    let main = cx.try_global::<MainWindow>().and_then(|main| main.0);
    match reopen_choice(main.map(|handle| handle.window_id()), &open) {
        ReopenChoice::Activate(_) => {
            // The choice only names a window that is in `open`, and every window this
            // application owns came from `open_main_window`, so the handle is here.
            let Some(handle) = main else {
                return Err("the main window handle is missing".to_owned());
            };
            match handle.update(cx, |view, window, cx| view.reveal_after_reopen(window, cx)) {
                Ok(()) => {
                    cx.activate(true);
                    Ok(())
                }
                // The window went away between the check and the activation. That is the
                // one case where a reopen still has to create one after all.
                Err(_) => {
                    cx.set_global(MainWindow(None));
                    open_main_window(cx)
                }
            }
        }
        ReopenChoice::OpenMain => {
            // A stale handle is dropped before the new window exists: keeping it would
            // make the next reopen look at a window that is gone.
            cx.set_global(MainWindow(None));
            open_main_window(cx)
        }
    }
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

    let handle = cx
        .open_window(options, |window, cx| {
            let view = cx.new(ShelluneView::new);
            // Give the shell the keyboard before the user clicks anything, so the
            // navigation shortcuts work in the first frame.
            window.focus(view.read(cx).root_focus());
            view
        })
        .map_err(|error| format!("{error:#}"))?;
    // Remembered so that a Dock click restores this window instead of building another
    // one, and so a reopen can tell a hidden window from a closed one (R13).
    cx.set_global(MainWindow(Some(handle)));
    Ok(())
}
