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
    /// A bin: remove a record.
    Trash,
    /// Three dots: more actions.
    More,
    /// A panel edge: collapse or expand the sidebar.
    Sidebar,
    /// A sun: the light theme.
    Sun,
    /// A moon: the dark theme.
    Moon,
    /// A screen: follow the system theme.
    Monitor,
    /// A bar: minimise the window.
    Minimize,
    /// A square: maximise the window.
    Maximize,
    /// Two squares: restore the window.
    Restore,
    /// A key: key file sign-in.
    Key,
    /// A padlock: password sign-in.
    Lock,
    /// A triangle: a command run after connecting.
    Play,
    /// An eye: show a hidden password.
    Eye,
    /// A struck eye: hide the password again.
    EyeOff,
    /// An open folder: choose a file.
    FolderOpen,
    /// A tick: a checked box.
    Check,
    /// A vertical split: panes side by side.
    SplitRight,
    /// A horizontal split: panes one above the other.
    SplitDown,
    /// Four corners: zoom the active pane.
    Zoom,
}

impl Icon {
    /// Every icon, for the asset listing and the tests.
    pub const ALL: [Self; 36] = [
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
        Self::Trash,
        Self::More,
        Self::Sidebar,
        Self::Sun,
        Self::Moon,
        Self::Monitor,
        Self::Minimize,
        Self::Maximize,
        Self::Restore,
        Self::Key,
        Self::Lock,
        Self::Play,
        Self::Eye,
        Self::EyeOff,
        Self::FolderOpen,
        Self::Check,
        Self::SplitRight,
        Self::SplitDown,
        Self::Zoom,
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
            Self::Trash => "icons/trash.svg",
            Self::More => "icons/more.svg",
            Self::Sidebar => "icons/sidebar.svg",
            Self::Sun => "icons/sun.svg",
            Self::Moon => "icons/moon.svg",
            Self::Monitor => "icons/monitor.svg",
            Self::Minimize => "icons/minimize.svg",
            Self::Maximize => "icons/maximize.svg",
            Self::Restore => "icons/restore.svg",
            Self::Key => "icons/key.svg",
            Self::Lock => "icons/lock.svg",
            Self::Play => "icons/play.svg",
            Self::Eye => "icons/eye.svg",
            Self::EyeOff => "icons/eye-off.svg",
            Self::FolderOpen => "icons/folder-open.svg",
            Self::Check => "icons/check.svg",
            Self::SplitRight => "icons/split-right.svg",
            Self::SplitDown => "icons/split-down.svg",
            Self::Zoom => "icons/zoom.svg",
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
            Self::Trash => {
                r#"<path d="M4 7h16"/><path d="M10 11v6"/><path d="M14 11v6"/><path d="M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12"/><path d="M9 7V4.5A1.5 1.5 0 0 1 10.5 3h3A1.5 1.5 0 0 1 15 4.5V7"/>"#
            }
            Self::More => r#"<path d="M5 12h.01"/><path d="M12 12h.01"/><path d="M19 12h.01"/>"#,
            Self::Sidebar => {
                r#"<rect x="3" y="4" width="18" height="16" rx="3"/><path d="M9 4v16"/>"#
            }
            Self::Sun => {
                r#"<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2"/><path d="M12 19.5v2"/><path d="M4.6 4.6l1.4 1.4"/><path d="M18 18l1.4 1.4"/><path d="M2.5 12h2"/><path d="M19.5 12h2"/><path d="M4.6 19.4L6 18"/><path d="M18 6l1.4-1.4"/>"#
            }
            Self::Moon => r#"<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>"#,
            Self::Monitor => {
                r#"<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8"/><path d="M12 16v4"/>"#
            }
            Self::Minimize => r#"<path d="M6 12h12"/>"#,
            Self::Maximize => r#"<rect x="5.5" y="5.5" width="13" height="13" rx="1.5"/>"#,
            Self::Restore => {
                r#"<rect x="5" y="8.5" width="10.5" height="10.5" rx="1.5"/><path d="M8.5 8.5V6.5A1.5 1.5 0 0 1 10 5h7.5A1.5 1.5 0 0 1 19 6.5V14a1.5 1.5 0 0 1-1.5 1.5h-2"/>"#
            }
            Self::Key => {
                r#"<circle cx="8" cy="15" r="4"/><path d="M11 12l8-8"/><path d="M16 7l3 3"/><path d="M14 9l2 2"/>"#
            }
            Self::Lock => {
                r#"<rect x="4.5" y="10.5" width="15" height="10" rx="2"/><path d="M8 10.5V7.5a4 4 0 0 1 8 0v3"/>"#
            }
            Self::Play => r#"<path d="M7 5l12 7-12 7z"/>"#,
            Self::Eye => {
                r#"<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>"#
            }
            Self::EyeOff => {
                r#"<path d="M3 3l18 18"/><path d="M10.6 5.6A9.6 9.6 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-3 3.8"/><path d="M6.6 6.6C4 8.3 2.5 12 2.5 12S6 18.5 12 18.5a9 9 0 0 0 4.4-1.1"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>"#
            }
            Self::FolderOpen => {
                r#"<path d="M3 7a2 2 0 0 1 2-2h4l2 2.5h7a2 2 0 0 1 2 2V11"/><path d="M3 7v10a2 2 0 0 0 2 2h12.5a2 2 0 0 0 1.9-1.4L21.5 12H7.2a2 2 0 0 0-1.9 1.4L3 19"/>"#
            }
            Self::Check => r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#,
            Self::SplitRight => {
                r#"<rect x="3.5" y="4" width="17" height="16" rx="2"/><path d="M12 4v16"/>"#
            }
            Self::SplitDown => {
                r#"<rect x="4" y="3.5" width="16" height="17" rx="2"/><path d="M4 12h16"/>"#
            }
            Self::Zoom => {
                r#"<path d="M9 4H5.5A1.5 1.5 0 0 0 4 5.5V9"/><path d="M15 4h3.5A1.5 1.5 0 0 1 20 5.5V9"/><path d="M20 15v3.5a1.5 1.5 0 0 1-1.5 1.5H15"/><path d="M9 20H5.5A1.5 1.5 0 0 1 4 18.5V15"/>"#
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

pub const APP_ICON: &str = "icons/shellune.png";

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if path == APP_ICON {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../assets/icons/shellune.png"
            ))));
        }
        Ok(Icon::from_path(path).map(|icon| Cow::Owned(icon.svg_source().into_bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .into_iter()
            .map(Icon::path)
            .chain(std::iter::once(APP_ICON))
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
