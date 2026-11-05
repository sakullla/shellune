//! GPUI application shell: product identity, window startup and the root view.
//!
//! Everything render-independent (hosts, sessions, transfers, safe errors) lives in
//! `shellune-core`; this module only composes it into GPUI state.

pub mod product;
mod view;

pub use view::ShelluneView;

use gpui::{
    App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size,
};

/// Starts the Shellune application and opens the main window.
pub fn run() {
    Application::new().run(|cx: &mut App| {
        if let Err(error) = open_main_window(cx) {
            eprintln!("shellune: failed to open the main window: {error}");
        }
        cx.activate(true);
    });
}

fn open_main_window(cx: &mut App) -> Result<(), String> {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(720.0), px(480.0))),
        titlebar: Some(TitlebarOptions {
            title: Some(product::NAME.into()),
            ..Default::default()
        }),
        app_id: Some(product::APP_ID.to_string()),
        ..Default::default()
    };

    cx.open_window(options, |_window, cx| cx.new(|_| ShelluneView::new()))
        .map(|_| ())
        .map_err(|error| format!("{error:#}"))
}
