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
pub mod hosts;
pub mod icons;
pub mod nav;
pub mod palette;
pub mod product;
pub mod remote;
pub mod terminal;
pub mod text_field;
pub mod theme;
pub mod transfers;
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
    CloseActiveTab, CycleTextSize, DismissNotice, GoToHosts, GoToLocalShells, GoToSessions,
    GoToTransfers, NewLocalShell, NextDestination, NextTab, OpenPalette, PreviousDestination,
    PreviousTab, SelectTab1, SelectTab2, SelectTab3, SelectTab4, SelectTab5, SelectTab6,
    SelectTab7, SelectTab8, SelectTab9, ToggleLanguage,
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
            SelectTab9
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
