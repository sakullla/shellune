//! The right-click menu of the file view.
//!
//! On an item it offers what can be done with that item - open a folder, copy a
//! file to the other side or download it elsewhere, rename, copy its path,
//! delete - and on the empty part of a list what can be done in that folder. Every
//! entry runs the same action as the buttons and keys that already exist, so the
//! menu adds a way to reach them, not a second behaviour.

use gpui::prelude::*;
use gpui::{Context, Div, Pixels, Point, SharedString, Window, div, px};

use super::{ShelluneView, overlay_layer};
use crate::app::copy;
use crate::app::files::FileSource;
use crate::app::icons::{self, Icon};
use crate::app::theme::{self, Tone, palette, radius, spacing};
use shellune_core::{FileKind, SessionId};

/// What the menu was opened on.
#[derive(Clone, Debug)]
pub(super) struct FilesMenu {
    pub(super) session: SessionId,
    pub(super) source: FileSource,
    /// The item, or `None` for the empty part of the list.
    pub(super) entry: Option<MenuEntry>,
    pub(super) position: Point<Pixels>,
}

#[derive(Clone, Debug)]
pub(super) struct MenuEntry {
    pub(super) name: String,
    pub(super) kind: FileKind,
    pub(super) folder: bool,
}

/// What a line of the menu does.
type Action = Box<dyn Fn(&mut ShelluneView, &mut Context<ShelluneView>)>;

/// One line of the menu: what it says, its icon, whether it warns, its selector
/// and what it does. `None` draws a separator.
type Line = Option<(&'static str, Icon, bool, &'static str, Action)>;

/// A path of this computer as its own programs write it: the file view keeps
/// Windows drives as `/C:/…`, which is not what Explorer or a shell accepts.
pub(super) fn native_local_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let drive =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    if drive {
        path[1..].replace('/', "\\")
    } else {
        path.to_owned()
    }
}

impl ShelluneView {
    /// Opens the menu for an item, which also becomes the selected one.
    pub(super) fn open_files_menu(
        &mut self,
        session: SessionId,
        source: FileSource,
        entry: Option<MenuEntry>,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if let Some(entry) = &entry {
            self.files.select(session, source, Some(entry.name.clone()));
        }
        self.files_menu = Some(FilesMenu {
            session,
            source,
            entry,
            position,
        });
        cx.notify();
    }

    fn close_files_menu(&mut self, cx: &mut Context<Self>) {
        if self.files_menu.take().is_some() {
            cx.notify();
        }
    }

    /// The full path of an item, or of the folder shown, as the user would paste it.
    fn files_path_text(
        &self,
        session: SessionId,
        source: FileSource,
        name: Option<&str>,
    ) -> Option<String> {
        let directory = self
            .files
            .browser_in(session, source)?
            .current_directory()?
            .to_owned();
        let path = match name {
            Some(name) => shellune_core::join_path(&directory, name),
            None => directory,
        };
        Some(match source {
            FileSource::Local => native_local_path(&path),
            FileSource::Remote => path,
        })
    }

    fn copy_files_path(
        &mut self,
        session: SessionId,
        source: FileSource,
        name: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.files_path_text(session, source, name) {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
            self.toast(Tone::Positive, copy::ux().toast_path_copied, Some(path), cx);
        }
    }

    /// The menu, when one is open.
    pub(super) fn files_menu_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let menu = self.files_menu.clone()?;
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let viewport = window.viewport_size();
        let id = menu.session;
        let source = menu.source;

        let mut lines: Vec<Line> = Vec::new();
        match &menu.entry {
            Some(entry) => {
                let name = entry.name.clone();
                let kind = entry.kind;
                if entry.folder {
                    let open = name.clone();
                    lines.push(Some((
                        ux.files_open,
                        Icon::FolderOpen,
                        false,
                        "files.menu.open",
                        Box::new(move |view, cx| view.files_enter(id, source, &open, cx)),
                    )));
                } else {
                    let across = name.clone();
                    match source {
                        FileSource::Remote => {
                            let download = name.clone();
                            lines.push(Some((
                                strings.files_to_local,
                                Icon::ChevronLeft,
                                false,
                                "files.menu.to_local",
                                Box::new(move |view, cx| {
                                    view.files_transfer_named(id, FileSource::Remote, &across, cx);
                                }),
                            )));
                            lines.push(Some((
                                strings.transfers_download,
                                Icon::Download,
                                false,
                                "files.menu.download",
                                Box::new(move |view, cx| {
                                    view.files_ask_download(id, Some(&download), cx)
                                }),
                            )));
                        }
                        FileSource::Local => lines.push(Some((
                            strings.files_to_remote,
                            Icon::Upload,
                            false,
                            "files.menu.to_remote",
                            Box::new(move |view, cx| {
                                view.files_transfer_named(id, FileSource::Local, &across, cx);
                            }),
                        ))),
                    }
                }
                lines.push(None);
                let rename = name.clone();
                lines.push(Some((
                    strings.files_rename,
                    Icon::Pencil,
                    false,
                    "files.menu.rename",
                    Box::new(move |view, cx| view.files_ask_rename(id, source, &rename, cx)),
                )));
                let copied = name.clone();
                lines.push(Some((
                    ux.files_copy_path,
                    Icon::Copy,
                    false,
                    "files.menu.copy_path",
                    Box::new(move |view, cx| view.copy_files_path(id, source, Some(&copied), cx)),
                )));
                lines.push(None);
                lines.push(Some((
                    strings.files_delete,
                    Icon::Trash,
                    true,
                    "files.menu.delete",
                    Box::new(move |view, cx| view.files_ask_delete(id, source, &name, kind, cx)),
                )));
            }
            None => {
                lines.push(Some((
                    strings.files_new,
                    Icon::FolderPlus,
                    false,
                    "files.menu.new",
                    Box::new(move |view, cx| view.files_new(id, source, cx)),
                )));
                if source == FileSource::Remote {
                    lines.push(Some((
                        strings.transfers_upload,
                        Icon::Upload,
                        false,
                        "files.menu.upload",
                        Box::new(move |view, cx| view.files_ask_upload(id, cx)),
                    )));
                }
                lines.push(Some((
                    strings.files_refresh,
                    Icon::Refresh,
                    false,
                    "files.menu.refresh",
                    Box::new(move |view, cx| view.files_refresh(id, source, cx)),
                )));
                lines.push(None);
                lines.push(Some((
                    ux.files_copy_path,
                    Icon::Copy,
                    false,
                    "files.menu.copy_path",
                    Box::new(move |view, cx| view.copy_files_path(id, source, None, cx)),
                )));
            }
        }

        let mut list = div()
            .flex()
            .flex_col()
            .min_w(metrics.scale(200.0))
            .p(metrics.scale(spacing::HAIRLINE))
            .rounded(px(radius::LARGE))
            .bg(theme::swatch(palette::SURFACE_RAISED))
            .border_1()
            .border_color(theme::swatch(palette::BORDER_STRONG))
            .shadow_lg()
            .debug_selector(|| "files.menu".to_owned());
        for line in lines {
            let Some((label, icon, danger, selector, action)) = line else {
                list = list.child(
                    div()
                        .h(px(1.0))
                        .mx(metrics.scale(spacing::HAIRLINE))
                        .my(metrics.scale(2.0))
                        .bg(theme::swatch(palette::BORDER)),
                );
                continue;
            };
            let hover = theme::swatch(if danger {
                Tone::Danger.tint()
            } else {
                palette::SURFACE_HOVER
            });
            let color = if danger {
                theme::swatch(palette::DANGER)
            } else {
                theme::swatch(palette::TEXT_PRIMARY)
            };
            list = list.child(
                div()
                    .id(selector)
                    .debug_selector(move || selector.to_owned())
                    .flex()
                    .items_center()
                    .gap(metrics.scale(spacing::TIGHT + 2.0))
                    .h(metrics.scale(30.0))
                    .px(metrics.scale(spacing::TIGHT + 2.0))
                    .rounded(px(radius::MEDIUM))
                    .text_size(metrics.label_size())
                    .text_color(color)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(icons::icon(
                        icon,
                        metrics.small_icon_size(),
                        if danger {
                            color
                        } else {
                            theme::swatch(palette::TEXT_SECONDARY)
                        },
                    ))
                    .child(SharedString::from(label))
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.files_menu = None;
                        action(view, cx);
                        cx.notify();
                    })),
            );
        }

        Some(
            overlay_layer().child(
                div()
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .child(
                        div()
                            .id("files.menu.backdrop")
                            .absolute()
                            .inset_0()
                            .occlude()
                            .on_any_mouse_down(cx.listener(|view, _event, _window, cx| {
                                view.close_files_menu(cx);
                            })),
                    )
                    .child(
                        gpui::anchored()
                            .position(menu.position)
                            .snap_to_window_with_margin(metrics.scale(spacing::TIGHT))
                            .child(div().occlude().child(list)),
                    ),
            ),
        )
    }

    /// The right-click handler of the empty part of a list.
    pub(super) fn files_blank_menu_listener(
        &self,
        id: SessionId,
        source: FileSource,
        cx: &mut Context<Self>,
    ) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |view, event: &gpui::MouseDownEvent, _window, cx| {
            view.open_files_menu(id, source, None, event.position, cx);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::native_local_path;

    #[test]
    fn a_windows_drive_path_is_written_the_windows_way() {
        assert_eq!(
            native_local_path("/C:/Users/me/a b.txt"),
            "C:\\Users\\me\\a b.txt"
        );
        assert_eq!(native_local_path("/home/me"), "/home/me");
        assert_eq!(native_local_path("/"), "/");
    }
}
