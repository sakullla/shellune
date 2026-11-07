//! The interface's icon set and the asset source that serves it.
//!
//! Icons are small line drawings on a 24-unit grid, embedded in the binary so the
//! application never depends on files beside the executable. GPUI renders an SVG as
//! an alpha mask and fills it with the element's text colour, so one drawing serves
//! every tone and both hover states.

use std::borrow::Cow;

use gpui::{AssetSource, Pixels, Rgba, SharedString, Svg, prelude::*, svg};

/// One drawing in the set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// A prompt: local shells and the "new shell" action.
    Terminal,
    /// A rack unit: saved hosts.
    Server,
    /// Stacked sheets: open sessions.
    Layers,
    /// Opposed arrows: file transfers.
    Transfer,
    /// A plus sign.
    Plus,
    /// A downward chevron for menus.
    ChevronDown,
    /// A rightward chevron.
    ChevronRight,
    /// A cross for closing.
    Close,
    /// A folder: the file view.
    Folder,
    /// A circular arrow: reconnect and retry.
    Refresh,
    /// Two sheets: copy.
    Copy,
    /// A globe: the interface language.
    Globe,
    /// Two letters of different sizes: text size.
    TextSize,
    /// A file with an arrow: import.
    Import,
    /// An arrow pointing right.
    ArrowRight,
    /// A magnifier: search.
    Search,
    /// A plug: connect.
    Plug,
}

impl Icon {
    /// Every icon, for the asset listing and the tests.
    pub const ALL: [Self; 17] = [
        Self::Terminal,
        Self::Server,
        Self::Layers,
        Self::Transfer,
        Self::Plus,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::Close,
        Self::Folder,
        Self::Refresh,
        Self::Copy,
        Self::Globe,
        Self::TextSize,
        Self::Import,
        Self::ArrowRight,
        Self::Search,
        Self::Plug,
    ];

    /// The asset path GPUI asks the source for.
    pub fn path(self) -> &'static str {
        match self {
            Self::Terminal => "icons/terminal.svg",
            Self::Server => "icons/server.svg",
            Self::Layers => "icons/layers.svg",
            Self::Transfer => "icons/transfer.svg",
            Self::Plus => "icons/plus.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::Close => "icons/close.svg",
            Self::Folder => "icons/folder.svg",
            Self::Refresh => "icons/refresh.svg",
            Self::Copy => "icons/copy.svg",
            Self::Globe => "icons/globe.svg",
            Self::TextSize => "icons/text-size.svg",
            Self::Import => "icons/import.svg",
            Self::ArrowRight => "icons/arrow-right.svg",
            Self::Search => "icons/search.svg",
            Self::Plug => "icons/plug.svg",
        }
    }

    /// The drawing itself: the inner elements of a 24×24 stroked SVG.
    fn body(self) -> &'static str {
        match self {
            Self::Terminal => {
                r#"<rect x="2.5" y="4" width="19" height="16" rx="3"/><path d="M7 9.5l3 2.5-3 2.5"/><path d="M12.5 15h4.5"/>"#
            }
            Self::Server => {
                r#"<rect x="3" y="3.5" width="18" height="7" rx="2"/><rect x="3" y="13.5" width="18" height="7" rx="2"/><path d="M7 7h.01"/><path d="M7 17h.01"/>"#
            }
            Self::Layers => {
                r#"<path d="M12 3l9 4.5-9 4.5-9-4.5z"/><path d="M3 12l9 4.5 9-4.5"/><path d="M3 16.5l9 4.5 9-4.5"/>"#
            }
            Self::Transfer => {
                r#"<path d="M7 20V4"/><path d="M3 8l4-4 4 4"/><path d="M17 4v16"/><path d="M21 16l-4 4-4-4"/>"#
            }
            Self::Plus => r#"<path d="M12 5v14"/><path d="M5 12h14"/>"#,
            Self::ChevronDown => r#"<path d="M6 9l6 6 6-6"/>"#,
            Self::ChevronRight => r#"<path d="M9 6l6 6-6 6"/>"#,
            Self::Close => r#"<path d="M6 6l12 12"/><path d="M18 6L6 18"/>"#,
            Self::Folder => {
                r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
            }
            Self::Refresh => r#"<path d="M20 12a8 8 0 1 1-2.34-5.66"/><path d="M20 4v5h-5"/>"#,
            Self::Copy => {
                r#"<rect x="8.5" y="8.5" width="12" height="12" rx="2"/><path d="M15.5 8.5V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7.5a2 2 0 0 0 2 2h2.5"/>"#
            }
            Self::Globe => {
                r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18"/><path d="M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9S9.5 5.6 12 3z"/>"#
            }
            Self::TextSize => {
                r#"<path d="M3 19l5-13 5 13"/><path d="M4.8 14.5h6.4"/><path d="M14.5 19l3.25-8 3.25 8"/><path d="M15.6 16.3h4.3"/>"#
            }
            Self::Import => {
                r#"<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/><path d="M12 11.5v6"/><path d="M9.5 15l2.5 2.5 2.5-2.5"/>"#
            }
            Self::ArrowRight => r#"<path d="M5 12h14"/><path d="M13 6l6 6-6 6"/>"#,
            Self::Search => r#"<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4"/>"#,
            Self::Plug => {
                r#"<path d="M9 3v5"/><path d="M15 3v5"/><path d="M6 8h12v3a6 6 0 0 1-12 0z"/><path d="M12 17v4"/>"#
            }
        }
    }

    /// The complete SVG document for this icon.
    pub fn svg_source(self) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
            self.body()
        )
    }

    fn from_path(path: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|icon| icon.path() == path)
    }
}

/// Serves the embedded icons to GPUI.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::from_path(path).map(|icon| Cow::Owned(icon.svg_source().into_bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .into_iter()
            .map(Icon::path)
            .filter(|candidate| candidate.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

/// An icon element of one size and colour.
pub fn icon(icon: Icon, size: Pixels, color: Rgba) -> Svg {
    svg()
        .path(icon.path())
        .flex_none()
        .size(size)
        .text_color(color)
}
