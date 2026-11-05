//! Root view of the main window.

use gpui::{Context, IntoElement, Render, SharedString, Window, div, prelude::*, px, rgb};
use shellune_core::{HostConfig, Session};

use crate::app::product;

const SURFACE: u32 = 0x11131a;
const SURFACE_RAISED: u32 = 0x191d27;
const BORDER: u32 = 0x262c3a;
const TEXT_PRIMARY: u32 = 0xe8ecf4;
const TEXT_MUTED: u32 = 0x8b93a5;
const ACCENT: u32 = 0x8aa8ff;

/// The application shell view.
///
/// The build baseline owns no hosts or sessions yet, so the view renders the real
/// (empty) application state instead of placeholder content pretending otherwise.
pub struct ShelluneView {
    version: SharedString,
    hosts: Vec<HostConfig>,
    sessions: Vec<Session>,
}

impl ShelluneView {
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            hosts: Vec::new(),
            sessions: Vec::new(),
        }
    }

    fn status_line(&self) -> SharedString {
        format!(
            "{} host{} · {} session{}",
            self.hosts.len(),
            if self.hosts.len() == 1 { "" } else { "s" },
            self.sessions.len(),
            if self.sessions.len() == 1 { "" } else { "s" },
        )
        .into()
    }
}

impl Default for ShelluneView {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for ShelluneView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let platform = SharedString::from(format!(
            "{} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        let status = self.status_line();

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT_PRIMARY))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_6()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(product::NAME),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(format!("v{}", self.version)),
                            ),
                    )
                    .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(status)),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .flex_col()
                    .justify_center()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .text_size(px(48.0))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(product::NAME),
                    )
                    .child(
                        div()
                            .text_lg()
                            .text_color(rgb(TEXT_MUTED))
                            .child(product::TAGLINE_EN),
                    )
                    .child(
                        div()
                            .text_base()
                            .text_color(rgb(ACCENT))
                            .child(product::TAGLINE_ZH),
                    )
                    .child(
                        div()
                            .mt_8()
                            .px_5()
                            .py_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(SURFACE_RAISED))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(TEXT_MUTED))
                                    .child("Application shell and domain core are ready."),
                            )
                            .child(div().text_sm().text_color(rgb(TEXT_MUTED)).child(
                                "Local shells, hosts, sessions and SFTP arrive in later tasks.",
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_6()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(SharedString::from(format!(
                        "{} ({}) · package {}",
                        product::NAME,
                        product::PRONUNCIATION,
                        product::PACKAGE_NAME
                    )))
                    .child(platform),
            )
    }
}
