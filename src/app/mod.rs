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
pub mod files;
pub mod forwarding;
pub mod glyphs;
pub mod hosts;
pub mod icons;
pub mod layout;
pub mod nav;
pub mod palette;
pub mod product;
pub mod remote;
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

pub use view::ShelluneView;

use gpui::{
    App, AppContext, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
    px, size,
};

use crate::app::actions::{
    CloseActiveTab, CycleTextSize, DismissNotice, FocusPaneDown, FocusPaneLeft, FocusPaneRight,
    FocusPaneUp, GoToHosts, GoToLocalShells, GoToSettings, MovePaneDown, MovePaneLeft,
    MovePaneRight, MovePaneUp, MoveTabLeft, MoveTabRight, NewLocalShell, NextDestination, NextTab,
    OpenPalette, PreviousDestination, PreviousTab, ResizePaneDown, ResizePaneLeft, ResizePaneRight,
    ResizePaneUp, SelectTab1, SelectTab2, SelectTab3, SelectTab4, SelectTab5, SelectTab6,
    SelectTab7, SelectTab8, SelectTab9, SplitPaneDown, SplitPaneRight, ToggleLanguage, ZoomPane,
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
            MoveTabRight
        ]
    );
}

/// Key context that scopes the shell's shortcuts.
pub const KEY_CONTEXT: &str = "Shellune";

/// Key context of a terminal pane's grid.
///
/// A terminal must receive the keys the shell expects; a shortcut bound to
/// [KEY_CONTEXT] would otherwise be taken before the pane ever sees the key. Two
/// matter most: Escape, because it is how Vim and less leave insert mode, and Ctrl+L,
/// which the shell reads as "clear the screen". Both used to be taken by a
/// window-wide shortcut - the notice bar and the language toggle respectively.
pub const TERMINAL_KEY_CONTEXT: &str = "Terminal";

/// The context the application's own shortcuts apply in: everywhere but a terminal.
///
/// Written as a predicate rather than a plain identifier because the pane is inside
/// the shell context too, so excluding it has to be said out loud. A shortcut that
/// only works outside a terminal belongs here; the language control in the status bar
/// is the in-terminal way to switch languages.
pub const APP_KEY_CONTEXT: &str = "Shellune && !Terminal";

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
        // Transfers and forwards are tools of a host now and the session list is the
        // tab strip, so only the three pages keep a number; settings also answer the
        // platform's usual comma.
        KeyBinding::new(
            &Destination::Settings.keystroke(platform),
            GoToSettings,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(&format!("{modifier}-,"), GoToSettings, Some(KEY_CONTEXT)),
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
        // Opening a shell works from inside a terminal too, so it is bound to the
        // shell context; the shifted form keeps Ctrl+T for the running program.
        KeyBinding::new(
            platform.new_shell_keystroke(),
            NewLocalShell,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            &format!("{modifier}-l"),
            ToggleLanguage,
            Some(APP_KEY_CONTEXT),
        ),
        // Escape and the language shortcut only belong to the application when no
        // terminal has the keyboard: inside one they are keys the running program
        // needs (Escape leaves Vim's insert mode, Ctrl+L clears the screen).
        KeyBinding::new("escape", DismissNotice, Some(APP_KEY_CONTEXT)),
        // The tab and palette shortcuts work from inside a terminal, like the new shell
        // shortcut: each is a combination no shell program reads (shifted, or with
        // Page Up/Down), so the running program loses nothing.
        KeyBinding::new(platform.palette_keystroke(), OpenPalette, Some(KEY_CONTEXT)),
        KeyBinding::new(
            platform.close_tab_keystroke(),
            CloseActiveTab,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            platform.tab_cycle_keystrokes().0,
            NextTab,
            Some(KEY_CONTEXT),
        ),
        KeyBinding::new(
            platform.tab_cycle_keystrokes().1,
            PreviousTab,
            Some(KEY_CONTEXT),
        ),
        // Pane keys use Alt so Escape, Ctrl+C and Ctrl+L stay with the shell.
        // Alt+arrows would otherwise be cursor sequences; taking them here is the
        // focus move, and the program does not also receive them.
        KeyBinding::new("alt-shift-=", SplitPaneRight, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift--", SplitPaneDown, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-z", ZoomPane, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-left", FocusPaneLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-right", FocusPaneRight, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-up", FocusPaneUp, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-down", FocusPaneDown, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-left", ResizePaneLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-right", ResizePaneRight, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-up", ResizePaneUp, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-down", ResizePaneDown, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-alt-left", MovePaneLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-alt-right", MovePaneRight, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-alt-up", MovePaneUp, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-alt-down", MovePaneDown, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-,", MoveTabLeft, Some(KEY_CONTEXT)),
        KeyBinding::new("alt-shift-.", MoveTabRight, Some(KEY_CONTEXT)),
    ]
    .into_iter()
    .chain(tab_jump_bindings(platform))
    .collect()
}

/// Number of tabs that have a jump shortcut.
pub const TAB_JUMP_COUNT: usize = 9;

/// Alt+1-9 (Ctrl+1-9 on macOS): straight to one tab.
fn tab_jump_bindings(platform: Platform) -> Vec<KeyBinding> {
    let modifier = platform.tab_jump_modifier();
    let key = |number: usize| format!("{modifier}-{number}");
    vec![
        KeyBinding::new(&key(1), SelectTab1, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(2), SelectTab2, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(3), SelectTab3, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(4), SelectTab4, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(5), SelectTab5, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(6), SelectTab6, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(7), SelectTab7, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(8), SelectTab8, Some(KEY_CONTEXT)),
        KeyBinding::new(&key(9), SelectTab9, Some(KEY_CONTEXT)),
    ]
}

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
