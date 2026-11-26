//! The window's frame: one top bar that is the title bar and the tab strip, the
//! hosts home (local shells and saved servers in one place), the shortcut capture layer,
//! and the dialogs that replaced inline forms and side pages: connecting to a host,
//! quick connect, port forwarding and transfers. The settings page itself is
//! ["settings"](super::settings), one category and one item at a time.
//!
//! The sidebar is gone. Everything a user switches between is a tab in the top bar:
//! the hosts home first, then one tab per open terminal. Settings sit at the right
//! end with search, next to the window buttons.

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Div, FontWeight, KeyDownEvent, Pixels, SharedString, Stateful,
    Window, div, px,
};

use shellune_core::{SessionId, SessionState, SnippetId};
use shellune_ssh::X11Status;

use super::{ShelluneView, SnippetNote, brand_mark, files_focus_ring, host_avatar, may_reconnect};
use crate::app::components::{self, Variant};
use crate::app::copy;
use crate::app::files::FileSource;
use crate::app::icons::{self, Icon};
use crate::app::nav::{Destination, Platform};
use crate::app::snippets::{SnippetEditor, body_preview};
use crate::app::terminal;
use crate::app::terminal::SnippetOffer;
use crate::app::text_field::TextField;
use crate::app::theme::{self, ThemeChoice, Tone, palette, radius, spacing};
use crate::app::workspace::{AttentionKind, NotifyKind, SectionState};

/// The least width a saved command's name and preview keep before its actions wrap.
const SNIPPET_DETAIL_MIN_WIDTH: f32 = 160.0;

/// The ring of the bar's colour around a notice badge, so the dot stands off the
/// control it marks; unscaled, like a border.
const NOTICE_BADGE_RIM: f32 = 1.5;

/// The edge of the app's mark at the start of the top bar, before zoom.
const BRAND_MARK_SIZE: f32 = 22.0;

/// The height of a segment of the files/terminal view switch, before zoom.
const VIEW_SEGMENT_HEIGHT: f32 = 20.0;

/// The padding of the files/terminal view switch's track, before zoom.
const VIEW_TRACK_PADDING: f32 = 1.0;

/// The height of the small actions on a session's card (save the host, close).
const SESSION_ACTION_HEIGHT: f32 = 22.0;

/// The edge of a square icon button of the top bar, before zoom.
const BAR_ICON_SIZE: f32 = 30.0;

/// The height of the short rule between the pinned hosts tab and the session tabs, and
/// between two session tabs that are neither current.
pub(super) const TAB_RULE_HEIGHT: f32 = 16.0;

/// The padding a segmented control's floor reserves on each side: the track's own
/// padding and a pixel of rounding room.
const SEGMENTED_FLOOR_PADDING: f32 = 3.0;

/// The edge of the dot of a connection step still to come, before zoom.
const STEP_DOT_SIZE: f32 = 6.0;

/// The edge of the dot that pulses beside the connection step that is running.
const PULSE_DOT_SIZE: f32 = 8.0;

/// The widest the card of a connection in flight grows.
const CONNECTION_CARD_MAX_WIDTH: f32 = 600.0;

/// A tool of a host or session that opens over the page instead of being a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Saved tunnels, for every host or one.
    Forwards,
    /// Uploads and downloads of every connection.
    Transfers,
    /// The saved shortcut commands, with the editor for one of them (R6).
    Snippets,
}

/// One of the session's controls on the bar under its terminal, besides its menu (R3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarControl {
    /// Start the ended session again.
    Reconnect,
    /// Save a quick connection as a host.
    SaveHost,
    /// Close a session that ended for good.
    Close,
    /// The terminal/files switch.
    Files,
    /// Split the active pane side by side.
    SplitRight,
    /// Split the active pane one above the other.
    SplitDown,
}

/// The session's half of the bar, before it is handed to the bar that draws it.
pub(super) struct SessionBarParts {
    /// Where the session is.
    pub left: Div,
    /// One control for each [`BarControl`] of
    /// [`ShelluneView::session_bar_controls`], in that order.
    pub controls: Vec<AnyElement>,
    /// The session's menu and what follows it, which always stay on the bar.
    pub pinned: Vec<AnyElement>,
}

/// The logical width the top row needs, less the window buttons it draws, for every
/// part at its full size (R2, R9): the brand, the hosts tab with its word and a
/// count, one session tab at [`theme::Metrics::tab_min_width`], the list of every tab,
/// the new-tab control, a caption gap and the page tools. A narrower window draws the
/// compact row ([`ShelluneView::header_compact`]).
const HEADER_FULL_ROW: f32 = 460.0;

/// The logical room one notice of the top row takes as an icon: a newer release, a
/// failed or conflicting sync, a run this window drives (R2, R9).
const NOTICE_ICON_ROOM: f32 = 30.0;

/// The logical room one notice of the top row takes at most as words, its gap
/// included: longer words end in an ellipsis, the whole sentence is in its tooltip.
const NOTICE_WORDS_ROOM: f32 = 200.0;

/// How the top row draws its notices (R2, R9).
///
/// The row's width is decided from the window's width, the text size and which notices
/// there are, never from a measured layout, so every form has a known width and the
/// current tab is never what gives way to a notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeForm {
    /// As words in a chip of at most [`NOTICE_WORDS_ROOM`].
    Words,
    /// As an icon of [`NOTICE_ICON_ROOM`] in the notice's tone, the words in its tooltip.
    Icon,
    /// Folded into a control the row already has - the settings button for a release
    /// or the sync, the task centre's entry for a run - as a mark in the notice's tone.
    /// A compact row has no room for anything more.
    Folded,
}

/// A notice of the top row drawn as an icon in its tone, the words in its tooltip.
pub(super) fn notice_icon(
    selector: &'static str,
    icon: Icon,
    tone: Tone,
    tooltip: String,
    metrics: theme::Metrics,
) -> Stateful<Div> {
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.scale(NOTICE_ICON_ROOM))
        .rounded(px(radius::MEDIUM))
        .bg(tone.chip_background())
        .cursor_pointer()
        .tooltip(components::tooltip(SharedString::from(tooltip)))
        .child(icons::icon(icon, metrics.icon_size(), tone.text()))
}

/// A notice of the top row drawn as words, never wider than [`NOTICE_WORDS_ROOM`].
pub(super) fn notice_words(
    selector: &'static str,
    label: impl Into<SharedString>,
    tone: Tone,
    metrics: theme::Metrics,
) -> Stateful<Div> {
    div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_none()
        .items_center()
        .max_w(metrics.scale(NOTICE_WORDS_ROOM - spacing::HAIRLINE))
        .mr(metrics.scale(spacing::HAIRLINE))
        .cursor_pointer()
        .child(components::chip_within(label, tone, metrics))
}

/// The mark a control carries for a notice folded into it, in the notice's tone and
/// shape, with the notice's own selector over the whole control so a press on it is a
/// press on the control.
pub(super) fn notice_badge(tone: Tone, selector: &'static str, metrics: theme::Metrics) -> Div {
    div()
        .absolute()
        .inset_0()
        .debug_selector(move || selector.to_owned())
        .child(
            div()
                .absolute()
                .top(metrics.scale(spacing::HALF))
                .right(metrics.scale(spacing::HALF))
                .p(px(NOTICE_BADGE_RIM))
                .rounded_full()
                .bg(theme::swatch(palette::CHROME))
                .child(components::status_dot(tone, metrics)),
        )
}

/// The pieces of one centred dialog; the dialog itself is the shared
/// [`components::modal`].
pub(super) use crate::app::components::{ModalBehaviour, ModalContent};

impl ShelluneView {
    // -----------------------------------------------------------------------
    // Top bar
    // -----------------------------------------------------------------------

    /// Whether the hosts home is what the content area shows.
    pub(super) fn home_is_shown(&self) -> bool {
        match self.workspace.destination() {
            Destination::Settings => false,
            Destination::LocalShells => !self.shows_terminal(Destination::LocalShells),
            _ => true,
        }
    }

    /// Whether the page under the head is the hosts home itself - the page
    /// [`Self::destination_panel`] falls back to - rather than the settings or a
    /// terminal.
    pub(super) fn home_page_shown(&self) -> bool {
        let destination = self.workspace.destination();
        destination != Destination::Settings && !self.shows_terminal(destination)
    }

    /// The window's head: the tab strip and, in the same surface under it, what the
    /// current session is doing (R12).
    ///
    /// One band, not two. The tab carries the name, its own colour and its state dot;
    /// this row carries what changes while that tab stays the same - the target, a
    /// state that is not "connected", a bridge or a save that needs an answer - and the
    /// session's actions. Drawing the name again under the tabs is what made the old
    /// window spend two rows on one object.
    pub(super) fn session_header(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        // One hairline closes the head along its whole width; the current tab is a
        // raised pill inside the head and no longer reaches it (ADR-7). Over the hosts
        // home there is none: the head, the sidebar and the frame around the page's
        // card are one chrome surface there, and the card's own edge is the line.
        let open_below = self.home_page_shown();
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .bg(theme::swatch(palette::CHROME))
            .debug_selector(|| "session.header".to_owned())
            .when(!open_below, |head| {
                head.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(components::HAIRLINE_WIDTH))
                        .bg(theme::swatch(palette::BORDER)),
                )
            })
            .child(self.top_bar(window, cx))
    }

    /// The top row: brand, the hosts tab, the terminal tabs, a new-tab control, the
    /// blank caption that drags the window, search, settings and (on Windows) the
    /// window buttons.
    ///
    /// It draws no surface of its own: it is the first row of the session header, which
    /// owns the band's background and its one bottom edge (R12).
    pub(super) fn top_bar(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let platform = Platform::current();
        let height = metrics.header_height();
        let compact = self.header_compact();

        let leading_room = if platform == Platform::MacOs {
            super::titlebar::traffic_light_room(metrics)
        } else {
            metrics.scale(spacing::SNUG)
        };
        let brand = self
            .drag_area("brand.drag")
            .flex()
            .flex_none()
            .items_center()
            .h(height)
            .pl(leading_room)
            // A compact row keeps the blank lead, which still drags the window, and
            // gives the mark's room to the current tab (R2, R9).
            .when(platform != Platform::MacOs && !compact, |row| {
                row.pr(metrics.scale(spacing::TIGHT))
                    .child(brand_mark(metrics.scale(BRAND_MARK_SIZE), metrics))
            });

        // The tabs stand in the middle of the head (ADR-7). The hosts tab is pinned and
        // never cut: the row's narrowest is at least its width. The session tabs scroll
        // beside it once they do not fit, and only they give up room (R2).
        // A short rule sets the pinned page apart from the sessions, as a browser sets
        // its pinned tabs apart: the hosts tab is a place, the others are connections.
        let has_tabs = !self.workspace.layout().tabs().is_empty();
        let home = div()
            .flex()
            .items_center()
            .flex_none()
            .h(height)
            .child(self.home_tab(cx))
            .when(has_tabs && !compact, |row| {
                row.child(
                    div()
                        .flex_none()
                        .mx(metrics.scale(spacing::HALF))
                        .w(px(components::HAIRLINE_WIDTH))
                        .h(metrics.scale(TAB_RULE_HEIGHT))
                        .bg(theme::swatch(palette::BORDER_STRONG))
                        .debug_selector(|| "nav.hosts.rule".to_owned()),
                )
            });
        let tabs = div()
            .flex()
            .items_center()
            .flex_initial()
            .min_w_0()
            .h(height)
            .overflow_hidden()
            .when(!self.workspace.layout().tabs().is_empty(), |row| {
                row.child(self.tab_strip(window, cx))
            });

        let trailing = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(metrics.scale(spacing::HALF))
            .pr(metrics.scale(spacing::HAIRLINE))
            .when(!compact && self.can_focus_mode(), |row| {
                let tooltip = match self.keymap.label("view.focus") {
                    Some(shortcut) => format!("{} ({shortcut})", ux.focus_mode_enter),
                    None => ux.focus_mode_enter.to_owned(),
                };
                row.child(
                    self.bar_icon("view.focus", Icon::Maximize, tooltip, self.focus_mode)
                        .on_click(cx.listener(|this, _event, window, cx| {
                            this.toggle_focus_mode(window, cx);
                        })),
                )
            })
            .when(self.security_topbar_lock_shown(), |row| {
                let security = copy::security_for(self.workspace.locale());
                let locked = self.vault_locked();
                let verifying = self.security_verifying();
                let tip = if verifying {
                    security.lock_state_verifying.to_owned()
                } else if locked {
                    security.topbar_unlock_tip.to_owned()
                } else {
                    security.topbar_lock_tip.to_owned()
                };
                row.child(
                    self.bar_icon(
                        "topbar.vault",
                        if locked { Icon::Lock } else { Icon::Unlock },
                        tip,
                        locked,
                    )
                    .relative()
                    .when(locked, |btn| {
                        btn.child(notice_badge(Tone::Warning, "topbar.vault.locked", metrics))
                    })
                    .on_click(cx.listener(|this, _event, _window, cx| {
                        if this.vault_locked() {
                            this.security_begin(Vec::new(), cx);
                        } else {
                            this.security_lock_now(cx);
                        }
                    })),
                )
            })
            .child(
                self.bar_icon(
                    "palette.open",
                    Icon::Search,
                    match self.keymap.label("palette.open") {
                        Some(shortcut) => format!("{} ({shortcut})", ux.search),
                        None => ux.search.to_owned(),
                    },
                    false,
                )
                .on_click(cx.listener(|this, _event, window, cx| {
                    this.toggle_palette(window, cx);
                })),
            )
            .child(self.settings_button(cx));

        let controls = self.window_controls(window, cx);
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(height)
            .w_full()
            .debug_selector(|| "topbar".to_owned())
            .child(brand)
            .child(home)
            .child(tabs)
            // Beside the strip rather than inside it: the strip clips what does not
            // fit, and the list of every tab has to stay reachable when it does (R2).
            .children(self.tab_list_control(cx))
            .child(self.new_tab_control(cx))
            .child(
                self.drag_area("header.drag")
                    .flex_1()
                    .h_full()
                    // A compact row keeps a pointer's width of caption, unscaled: the
                    // pointer does not grow with the text (R9).
                    .min_w(if compact {
                        px(theme::HIT_TARGET)
                    } else {
                        metrics.scale(theme::HIT_TARGET)
                    }),
            )
            // The task library belongs to the home page; an active run has its own indicator.
            .children(self.batch_topbar_control(cx))
            // A failed sync or a conflict waiting for an answer is said here, on every
            // page, and a press opens the sync settings (R8, R10).
            .children(self.sync_topbar_marker(cx))
            // A newer release is said here on every page, and a press opens its card
            // in the settings (R2).
            .children(self.update_topbar_chip(cx))
            .child(trailing)
            .children(controls)
    }

    /// What the session in front says about itself and what can be done to it, as the
    /// two halves of the bar under the terminal (R12).
    ///
    /// The head above is the tabs and the application's own buttons, the same for a
    /// local shell and a server; the session lives at the bottom, beside the terminal
    /// tools, as in an editor's status line. The left half is where it is - the
    /// target, and the state when it is news; the right half is what it does - files,
    /// splitting, the reconnect or the save that is waiting, and the menu. The tab
    /// closes the session, so there is no second Close here.
    pub(super) fn session_bar_parts(&self, cx: &mut Context<Self>) -> Option<SessionBarParts> {
        let id = self.bar_session()?;
        let session = self.workspace.session(id)?;
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        // One projection answers what this session is doing; the bar draws it and does
        // not decide any of it again (R17).
        let status = crate::app::workspace::SessionStatus::of(session);
        let live = status.phase().is_live();
        let target = self.target_of(session);
        let state_label = status.headline(self.workspace.locale());
        let tone = status.tone();
        let can_reconnect = may_reconnect(session);
        // Files are an SSH capability: the entry exists only for a session whose
        // protocol has one, so a desktop or serial session has nothing to hide (R18).
        let browsable =
            self.allows(id, shellune_core::Capability::Files) && self.files.can_browse(id);
        let files_open = self.files.is_open(id);
        let unsaved = self.unsaved.iter().any(|(session, _)| *session == id);
        // A desktop surface is shown in the same tab and by the same bar; only a
        // session that has neither a terminal nor a picture has nothing to open.
        let has_terminal = self.pane_for(id).is_some() || self.desktop_for(id).is_some();
        // While the connection card or an active failure/reconnecting banner is on screen,
        // that surface is the single place stating the status and next action; the bar
        // avoids duplicating the status chip or repeating Reconnect/Close until dismissed (R5).
        let card = self.connection_inline();
        let banner_shown = self.banner_shown(id, cx);
        // "The session is over" is the projection's answer, not a second match on the
        // state here: a fresh variant must not turn into a silently different page.
        let ended = status.phase().is_over();
        let can_reconnect = can_reconnect && !card && !banner_shown;

        let left = div()
            .flex()
            .items_center()
            .gap(metrics.scale(spacing::TIGHT))
            .min_w_0()
            .overflow_hidden()
            .debug_selector(|| "session.context".to_owned())
            .child(components::status_dot(tone, metrics))
            // What the session is, before where it is: the bar under a server and the
            // bar under this computer's shell read alike, and only the target's shape
            // told them apart. The words are the tab list's and the palette's.
            .child({
                let (label, remote, word) =
                    super::overlays::kind_badge_parts(super::PaneKind::of(session.kind()), strings);
                components::kind_badge(label, remote, metrics)
                    .debug_selector(move || format!("session.kind.{word}"))
            })
            .child(
                self.copyable_text(
                    "session.target",
                    target,
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(terminal::monospace_family())
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY)),
                    cx,
                ),
            )
            .when(!live && !banner_shown, |row| {
                if self.security_verifying_for_desktop(id) {
                    let security = copy::security_for(self.workspace.locale());
                    row.child(
                        components::chip(
                            SharedString::from(security.lock_state_verifying),
                            Tone::Warning,
                            metrics,
                        )
                        .debug_selector(|| "session.verifying".to_owned()),
                    )
                } else {
                    row.child(components::chip(state_label, tone, metrics))
                }
            })
            // "X11 off" is the default and says nothing; only a bridge that is up,
            // coming up or failed is worth a chip.
            .when_some(
                self.x11
                    .get(&id)
                    .filter(|status| !matches!(status, X11Status::Off))
                    .cloned(),
                |row, status| row.child(self.x11_chip(&status, metrics)),
            )
            .when(unsaved, |row| {
                row.child(
                    div()
                        .flex_none()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(strings.quick_unsaved_note),
                )
            });

        // Terminal and files are two views of one connection: a small switch, icons
        // named by their tooltips.
        let files_button = browsable.then(|| {
            let view_segment =
                move |selector: &'static str, icon: Icon, label: &'static str, selected: bool| {
                    let color = theme::swatch(if selected {
                        palette::TEXT_PRIMARY
                    } else {
                        palette::TEXT_SECONDARY
                    });
                    components::segment("", selected, metrics)
                        .h(metrics.scale(VIEW_SEGMENT_HEIGHT))
                        .px(metrics.scale(spacing::HAIRLINE + 2.0))
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::HAIRLINE))
                        .child(icons::icon(icon, metrics.small_icon_size(), color))
                        .child(label)
                        .id(selector)
                        .debug_selector(move || selector.to_owned())
                };
            components::segmented(metrics)
                .flex_none()
                .flex_nowrap()
                .p(metrics.scale(VIEW_TRACK_PADDING))
                .child(
                    view_segment(
                        "files.close",
                        Icon::Terminal,
                        ux.settings_terminal,
                        !files_open,
                    )
                    // The way back to the terminal is a stop of the file view's keyboard
                    // walk, so it can be reached with Tab and pressed with Enter (R4).
                    .track_focus(&self.files_close_focus)
                    .focus(files_focus_ring)
                    .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                        if view.files_key(id, FileSource::Remote, event, window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .on_click(cx.listener(move |view, _event, window, cx| {
                        if view.files.is_open(id) {
                            view.toggle_files(id, cx);
                            view.reclaim_focus(window, cx);
                        }
                    })),
                )
                .child(
                    view_segment(
                        "session.files",
                        Icon::Folder,
                        strings.files_open,
                        files_open,
                    )
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        if !view.files.is_open(id) {
                            view.toggle_files(id, cx);
                        }
                    })),
                )
                // A window with room to dock can share its width with the terminal or
                // give it all to the files; the switch says what the next press gives,
                // and it lives in this bar so that neither arrangement moves the listing
                // (R14). A narrower window is not offered it: there the files already
                // have the terminal's place, so the press would change nothing but its
                // own label.
                .children((files_open && self.files_dock_capable()).then(|| {
                    let (selector, icon, label) = if self.files_focused() {
                        ("files.mode.dock", Icon::Terminal, strings.files_mode_dock)
                    } else {
                        ("files.mode.focus", Icon::Maximize, strings.files_mode_focus)
                    };
                    view_segment(selector, icon, label, self.files_focused()).on_click(cx.listener(
                        move |view, _event, _window, cx| {
                            view.files_toggle_focus(cx);
                        },
                    ))
                }))
        });

        // The right half is a list of separate controls rather than one row: the bar
        // under a terminal lays them out itself, so in a pane too narrow for all of them a
        // control that does not fit is left out whole instead of being cut through (R3).
        // The menu is apart from them and always stays: it offers what was left out.
        let kinds = self.session_bar_controls(id, cx);
        debug_assert_eq!(
            kinds.contains(&BarControl::Reconnect),
            can_reconnect,
            "the bar's controls and its reconnect agree"
        );
        let mut right: Vec<AnyElement> = Vec::new();
        if can_reconnect {
            // The card over the terminal names the step in words; the bar's entry is the
            // icon and its tooltip, a control the size of its neighbours, so it is still
            // whole in a split pane at 200% where a labelled button was cut through.
            let tooltip = match self.keymap.label("session.reconnect") {
                Some(shortcut) => format!("{} ({shortcut})", strings.session_reconnect),
                None => strings.session_reconnect.to_owned(),
            };
            right.push(
                self.bar_icon("session.reconnect", Icon::Refresh, tooltip, false)
                    .size(metrics.hit_target())
                    .flex_none()
                    .on_click(cx.listener(move |view, _event, _window, cx| {
                        view.reconnect_session(id, cx);
                    }))
                    .into_any_element(),
            );
        }
        if unsaved {
            right.push(
                self.icon_text_button(
                    SharedString::from("session.save_host"),
                    Icon::Server,
                    strings.quick_save_host,
                    Variant::Primary,
                    cx,
                    move |view, cx| view.save_quick_host(id, cx),
                )
                .h(metrics.scale(SESSION_ACTION_HEIGHT))
                .text_size(metrics.caption_size())
                .into_any_element(),
            );
        }
        // A session that ended for good has one thing left to do; when the banner already
        // presents Close, avoid duplicating it in the bar until the banner is dismissed.
        if ended && !can_reconnect && !unsaved && !card && !banner_shown {
            right.push(
                self.icon_text_button(
                    SharedString::from("session.close"),
                    Icon::Close,
                    strings.session_close,
                    Variant::Secondary,
                    cx,
                    move |view, cx| view.request_close_session(id, cx),
                )
                .h(metrics.scale(SESSION_ACTION_HEIGHT))
                .text_size(metrics.caption_size())
                .into_any_element(),
            );
        }
        if let Some(button) = files_button {
            right.push(button.into_any_element());
        }
        if has_terminal {
            right.extend(
                self.split_buttons(cx)
                    .into_iter()
                    .map(IntoElement::into_any_element),
            );
        }
        debug_assert_eq!(
            right.len(),
            kinds.len(),
            "one control is drawn for each the session's menu may stand in for"
        );
        let hidden = self.session_bar_hidden(id, cx).len();
        let more_tooltip = if hidden > 0 {
            format!("{} (+{hidden})", strings.session_more)
        } else {
            strings.session_more.to_owned()
        };
        let mut pinned: Vec<AnyElement> = Vec::new();
        pinned.push(
            self.bar_icon(
                "session.more",
                Icon::More,
                more_tooltip,
                self.session_more.is_some(),
            )
            .size(metrics.hit_target())
            .flex_none()
            // Opened on the press, like the tab menu, so the terminal keeps the
            // keyboard and Escape is still answered (R5).
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(move |view, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    view.open_session_more(id, event.position, window, cx);
                }),
            )
            .into_any_element(),
        );
        Some(SessionBarParts {
            left,
            controls: right,
            pinned,
        })
    }

    /// The session the bar under the terminal speaks for.
    pub(super) fn bar_session(&self) -> Option<SessionId> {
        if self.workspace.layout().active_tab().is_some() {
            self.workspace.layout().active_session()
        } else {
            self.active
        }
    }

    /// Which controls the bar draws for session `id`, in the order it draws them,
    /// besides its menu (R3).
    ///
    /// The bar and the session's menu both read this list: a control that a narrow bar
    /// has no room for is offered in the menu instead (R9).
    pub(super) fn session_bar_controls(&self, id: SessionId, cx: &App) -> Vec<BarControl> {
        let Some(session) = self.workspace.session(id) else {
            return Vec::new();
        };
        let status = crate::app::workspace::SessionStatus::of(session);
        let card = self.connection_inline();
        let banner_shown = self.banner_shown(id, cx);
        let can_reconnect = may_reconnect(session) && !card && !banner_shown;
        let unsaved = self.unsaved.iter().any(|(session, _)| *session == id);
        let ended = status.phase().is_over();
        let browsable =
            self.allows(id, shellune_core::Capability::Files) && self.files.can_browse(id);
        let has_terminal = self.pane_for(id).is_some() || self.desktop_for(id).is_some();
        let mut controls = Vec::new();
        if can_reconnect {
            controls.push(BarControl::Reconnect);
        }
        if unsaved {
            controls.push(BarControl::SaveHost);
        }
        if ended && !can_reconnect && !unsaved && !card && !banner_shown {
            controls.push(BarControl::Close);
        }
        if browsable {
            controls.push(BarControl::Files);
        }
        if has_terminal && self.workspace.layout().active_pane().is_some() {
            controls.extend([BarControl::SplitRight, BarControl::SplitDown]);
        }
        controls
    }

    /// The controls of session `id` that the bar under its terminal had no room for at
    /// its last layout, which the session's menu offers instead (R3, R9).
    pub(super) fn session_bar_hidden(&self, id: SessionId, cx: &App) -> Vec<BarControl> {
        if self.bar_session() != Some(id) {
            return Vec::new();
        }
        let Some(pane) = self.pane_for(id) else {
            return Vec::new();
        };
        let hidden = pane.read(cx).bar_hidden_controls();
        if hidden == 0 {
            return Vec::new();
        }
        let controls = self.session_bar_controls(id, cx);
        let first = controls.len().saturating_sub(hidden);
        controls[first..].to_vec()
    }

    /// The selectors of the bar controls of session `id` that its bar had no room for,
    /// for a test.
    #[cfg(test)]
    pub fn session_bar_hidden_for_test(&self, id: SessionId, cx: &App) -> Vec<&'static str> {
        self.session_bar_hidden(id, cx)
            .into_iter()
            .map(|control| match control {
                BarControl::Reconnect => "session.reconnect",
                BarControl::SaveHost => "session.save_host",
                BarControl::Close => "session.close",
                BarControl::Files => "session.files",
                BarControl::SplitRight => "pane.split.right",
                BarControl::SplitDown => "pane.split.down",
            })
            .collect()
    }

    /// The session bar on its own, for when no terminal is on screen to carry it: the
    /// file view, or a session that has no terminal (yet).
    pub(super) fn standalone_session_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.focus_mode {
            return None;
        }
        let SessionBarParts {
            left,
            controls,
            pinned,
        } = self.session_bar_parts(cx)?;
        let metrics = self.metrics();
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_between()
                .gap(metrics.scale(spacing::TIGHT))
                .min_h(
                    metrics
                        .hit_target()
                        .max(metrics.scale(terminal::SHELL_BAR_HEIGHT)),
                )
                .pl(metrics.scale(spacing::BASE - 2.0))
                .pr(metrics.scale(spacing::HAIRLINE))
                .bg(theme::swatch(palette::CHROME))
                .border_t_1()
                .border_color(theme::faded(theme::swatch(palette::BORDER), 0.8))
                .debug_selector(|| "session.bar".to_owned())
                .child(left.flex_1())
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(metrics.scale(spacing::HALF))
                        .children(controls)
                        .children(pinned),
                ),
        )
    }

    /// Whether the top row is too narrow for every part at its full size, so it draws
    /// the compact form (R2, R9).
    ///
    /// That is 200% text in a small window: the brand, the hosts tab's word and count,
    /// a session tab of the strip's minimum width, the strip's controls, a caption gap,
    /// the page tools and the window buttons need more than the window has. The
    /// compact row drops the brand mark and the new-tab chevron (its shells are in the
    /// palette and on the hosts home's own menu), shows the hosts tab as its icon, the
    /// page tools at the pointer target's size, and only the current session tab; the
    /// list of every tab beside the strip reaches the rest; the notices the full row
    /// would draw beside the page tools are folded into its controls
    /// ([`NoticeForm::Folded`]). It is decided from the window's width, the text size
    /// and which notices there are, never from a measured layout, so the row cannot flip
    /// between its two forms from one frame to the next.
    pub(super) fn header_compact(&self) -> bool {
        let metrics = self.metrics();
        // Each notice the full row would draw takes at least an icon's room beside the
        // parts above; without that room the row folds them into its controls instead
        // of letting the current tab give way (R2).
        let notices =
            (self.topbar_notice_count() + usize::from(self.security_topbar_lock_shown())) as f32;
        self.viewport_width
            < metrics.scale(HEADER_FULL_ROW + NOTICE_ICON_ROOM * notices)
                + super::titlebar::window_controls_width(Platform::current())
    }

    /// How many notices the top row has to show: a newer release, a failed or
    /// conflicting sync, a run this window drives (R2).
    fn topbar_notice_count(&self) -> usize {
        usize::from(self.update_notice().is_some())
            + usize::from(self.sync_notice().is_some())
            + usize::from(self.batch_notice().is_some())
    }

    /// How the top row draws its notices (R2, R9).
    ///
    /// A compact row folds them into its controls. A full row draws each as words when
    /// the row has [`NOTICE_WORDS_ROOM`] for every one of them beside its own parts, and
    /// as an icon otherwise; [`Self::header_compact`] already counts that icon's room,
    /// so neither form takes the room of the current tab.
    pub(super) fn topbar_notice_form(&self) -> NoticeForm {
        if self.header_compact() {
            return NoticeForm::Folded;
        }
        let metrics = self.metrics();
        let notices = self.topbar_notice_count() as f32;
        let words = metrics.scale(HEADER_FULL_ROW + NOTICE_WORDS_ROOM * notices)
            + super::titlebar::window_controls_width(Platform::current());
        if self.viewport_width >= words.max(metrics.scale(super::update::TOPBAR_FULL_CHIP_WIDTH)) {
            NoticeForm::Words
        } else {
            NoticeForm::Icon
        }
    }

    /// The top row's settings button.
    ///
    /// In a compact row it also carries the notices that lead into the settings - a
    /// failed or conflicting sync, a newer release - as a mark in the first one's tone,
    /// their words in its tooltip, and a press that opens the page the first one leads
    /// to: the sync settings, else the update card. A full row draws them on their own
    /// (R2, R9).
    fn settings_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let ux = copy::ux();
        let metrics = self.metrics();
        let folded = self.topbar_notice_form() == NoticeForm::Folded;
        let sync = self.sync_notice().filter(|_| folded);
        let update = self.update_notice().filter(|_| folded);
        let mut tooltip = match crate::app::keymap::page_action(Destination::Settings)
            .and_then(|spec| self.keymap.label(spec.id))
        {
            Some(shortcut) => format!("{} ({shortcut})", ux.settings_title),
            None => ux.settings_title.to_owned(),
        };
        for label in sync
            .map(|(label, _)| label.to_owned())
            .into_iter()
            .chain(update.clone())
        {
            tooltip.push_str(" \u{b7} ");
            tooltip.push_str(&label);
        }
        let leads_to_sync = sync.is_some();
        let leads_to_update = update.is_some();
        self.bar_icon(
            "nav.settings",
            Icon::Settings,
            tooltip,
            self.workspace.destination() == Destination::Settings,
        )
        .relative()
        .when_some(sync, |button, (_, tone)| {
            button.child(notice_badge(tone, "topbar.sync", metrics))
        })
        .when(leads_to_update, |button| {
            // One mark is enough: a second one would sit on the first.
            let tone = sync.map_or(Tone::Accent, |(_, tone)| tone);
            button.child(notice_badge(tone, "topbar.update", metrics))
        })
        .on_click(cx.listener(move |this, _event, window, cx| {
            if leads_to_sync {
                this.open_sync_settings(window, cx);
            } else if leads_to_update {
                this.open_update_settings(window, cx);
            } else {
                this.select(Destination::Settings, window, cx);
            }
        }))
    }

    /// How the top row draws its notices, for a test: "words", "icon" or "folded".
    #[cfg(test)]
    pub fn topbar_notice_form_for_test(&self) -> &'static str {
        match self.topbar_notice_form() {
            NoticeForm::Words => "words",
            NoticeForm::Icon => "icon",
            NoticeForm::Folded => "folded",
        }
    }

    /// Whether the top row draws its compact form, for a test.
    #[cfg(test)]
    pub fn header_compact_for_test(&self) -> bool {
        self.header_compact()
    }

    /// The narrowest a session tab becomes in the row as it is drawn now (R2).
    pub(super) fn tab_min_width(&self) -> Pixels {
        if self.header_compact() {
            self.metrics().tab_compact_min_width()
        } else {
            self.metrics().tab_min_width()
        }
    }

    /// The narrowest a session tab becomes in the row as it is drawn now, for a test.
    #[cfg(test)]
    pub fn tab_min_width_for_test(&self) -> Pixels {
        self.tab_min_width()
    }

    /// A square icon button of the top bar.
    fn bar_icon(
        &self,
        id: &'static str,
        icon: Icon,
        tooltip: String,
        selected: bool,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let size = if self.header_compact() {
            metrics.hit_target()
        } else {
            metrics.scale(BAR_ICON_SIZE)
        };
        components::icon_only_button(icon, metrics)
            .size(size)
            .rounded(px(radius::MEDIUM))
            .when(selected, |button| {
                button.bg(theme::swatch(palette::SURFACE_SELECTED))
            })
            .id(id)
            .debug_selector(move || id.to_owned())
            .tooltip(components::tooltip(SharedString::from(tooltip)))
    }

    /// The pinned first tab: the hosts home.
    ///
    /// It has the shape of every other tab (R2) and the parts a page has: no state dot
    /// and no close button, since the home is not a session and cannot be closed. It
    /// sits outside the scrolling strip, so it is always one click away.
    ///
    /// It is its icon alone at every width, as a browser's pinned tab is: the sidebar
    /// of the page it opens already heads its list "All hosts", so the word beside the
    /// icon only repeated it and took the session tabs' room. The name, the count and
    /// the shortcut are in its tooltip, so nothing of it is lost (R2, R9).
    fn home_tab(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let selected = self.home_is_shown();
        // Every saved record the page lists, desktop and serial ones too.
        let count = self.book.hosts().len() + self.book.connections().len();
        // The hint reads the binding in force, so a rebound or unbound shortcut is
        // described as it really is (R5).
        let shortcut = crate::app::keymap::page_action(Destination::Hosts)
            .and_then(|spec| self.keymap.label(spec.id));
        // The count of saved records is the sidebar's: beside the word it read as part
        // of the name ("Hosts 1"), so the tab names the page and its tooltip counts.
        let name = if count > 0 {
            format!("{} ({count})", strings.nav_hosts)
        } else {
            strings.nav_hosts.to_owned()
        };
        let tooltip = match shortcut {
            Some(shortcut) => format!("{name} · {shortcut}"),
            None => name,
        };
        super::browser_tab("nav.hosts", "nav.hosts".to_owned(), selected, metrics)
            // Pinned and always there, so it is only as wide as its icon: the room it
            // does not take is the session tabs'. The padding is the same on both sides
            // so the icon sits in the middle of a square-ish target.
            .flex_none()
            .justify_center()
            .min_w(metrics.tab_height())
            .pr(metrics.scale(spacing::SNUG - 2.0))
            .tooltip(components::tooltip(SharedString::from(tooltip)))
            .on_click(cx.listener(|this, _event, window, cx| {
                this.select(Destination::Hosts, window, cx);
            }))
            .child(super::tab_icon(Icon::Server, selected, metrics))
    }

    /// "+" opens the default shell in a new tab; the chevron lists every shell, quick
    /// connect and the saved hosts.
    ///
    /// A compact row has "+" alone: the shells are in the palette and on the hosts
    /// home's own menu, one click away on the hosts tab, and quick connect is on that
    /// page too (R9).
    fn new_tab_control(&self, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let compact = self.header_compact();
        let open = self.shell_menu_open && !self.shell_menu_home;
        let size = if compact {
            metrics.hit_target()
        } else {
            metrics.compact_control_height()
        };
        let new_shell = self.keymap.label("shell.new");
        // One control: the press opens the launcher - the shells, the saved hosts and
        // a typed address in one searchable list - and Enter there opens the default
        // shell, so a new local tab is still one press and a key away. The shortcut
        // opens the default shell directly.
        let tooltip = match new_shell {
            Some(shortcut) => format!("{} · {} {shortcut}", ux.new_tab_menu, ux.new_tab),
            None => ux.new_tab_menu.to_owned(),
        };
        let control = div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .ml(metrics.scale(spacing::HAIRLINE))
            .child(
                components::icon_only_button(Icon::Plus, metrics)
                    .size(size)
                    .rounded(px(radius::MEDIUM))
                    .when(open, |button| {
                        button.bg(theme::swatch(palette::SURFACE_SELECTED))
                    })
                    .id("header.primary")
                    .debug_selector(|| "header.primary".to_owned())
                    .when(!open, |button| {
                        button.tooltip(components::tooltip(SharedString::from(tooltip)))
                    })
                    .on_click(cx.listener(|this, _event, window, cx| {
                        if this.shell_menu_open && !this.shell_menu_home {
                            this.close_shell_menu(cx);
                        } else {
                            this.open_launcher(window, cx);
                        }
                    })),
            );
        // Records where the control is drawn, for the menu that hangs from it.
        let anchor = self.shell_menu_anchor.clone();
        control.child(
            gpui::canvas(
                move |bounds, _window, _cx| anchor.set(bounds),
                |_, _, _, _| {},
            )
            .absolute()
            .inset_0(),
        )
    }

    /// The capture layer: what the next combination pressed will be bound to.
    ///
    /// It sits over the window and reads the keyboard through an interceptor, so the
    /// combination is seen before any action it is already bound to runs.
    pub(super) fn key_capture_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let capture = self.key_capture.as_ref()?;
        let spec = crate::app::keymap::action(capture.action)?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let locale = self.workspace.locale();
        let platform = self.keymap.platform();
        let current = self.keymap.label(spec.id);
        let (header, subtitle) = self.modal_header(
            Some(Icon::Key),
            ux.keys_capture_title.to_owned(),
            None,
            "keys.capture.close",
            cx,
            |this, cx| this.keys_capture_cancel(cx),
        );
        // The action being bound and the combination in force now, then the refusal of
        // the last combination pressed, if any, and how to leave.
        let body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pt(metrics.scale(spacing::TIGHT))
            .pb(metrics.scale(spacing::BASE))
            .debug_selector(|| "keys.capture".to_owned())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::TIGHT))
                    .px(metrics.scale(spacing::BASE))
                    .py(metrics.scale(spacing::SNUG))
                    .rounded(px(radius::MEDIUM))
                    .bg(theme::swatch(palette::CHROME))
                    .border_1()
                    .border_color(theme::swatch(palette::BORDER))
                    .child(
                        div()
                            .whitespace_normal()
                            .text_size(metrics.body_size())
                            .font_weight(metrics.medium_weight())
                            .text_color(theme::swatch(palette::TEXT_PRIMARY))
                            .child(SharedString::from(spec.name(locale))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(metrics.scale(spacing::TIGHT))
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .child(match current {
                                Some(current) => {
                                    components::kbd(current, metrics).into_any_element()
                                }
                                None => div()
                                    .child(SharedString::from(ux.keys_unbound))
                                    .into_any_element(),
                            }),
                    ),
            )
            .when_some(capture.problem.as_ref(), |card, problem| {
                card.child(
                    components::banner(Tone::Danger, metrics)
                        .debug_selector(|| "keys.capture.error".to_owned())
                        .child(SharedString::from(problem.text(locale, platform))),
                )
            })
            .child(
                div()
                    .whitespace_normal()
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_MUTED))
                    .child(ux.keys_capture_hint),
            )
            .into_any_element();
        let footer = components::modal_footer(metrics).child(
            components::compact_button(ux.keys_capture_cancel, Variant::Ghost, metrics)
                .h(metrics.control_height())
                .id("keys.capture.cancel")
                .debug_selector(|| "keys.capture.cancel".to_owned())
                .on_click(cx.listener(|this, _event, _window, cx| {
                    this.keys_capture_cancel(cx);
                })),
        );
        // Nothing typed can be lost here, so a press outside the dialog is a way out,
        // as it was when the capture was a card.
        let dismiss: components::ModalDismiss = Box::new(cx.listener(
            |this, _event: &gpui::MouseDownEvent, _window, cx| {
                this.keys_capture_cancel(cx);
            },
        ));
        Some(self.modal_with(
            window,
            "keys.capture.dialog",
            460.0,
            ModalContent {
                header,
                subtitle,
                body,
                footer: Some(footer),
            },
            ModalBehaviour {
                scroll: None,
                dismiss: Some(dismiss),
                ..Default::default()
            },
        ))
    }

    /// A segmented control whose segments call `pick` with their index.
    pub(super) fn segments(
        &self,
        id: &'static str,
        options: Vec<(String, bool)>,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Self, usize, &mut Context<Self>) + Clone + 'static,
    ) -> Div {
        let metrics = self.metrics();
        // The track's own floor is the sum of its segments' floors, capped at what a
        // settings row has at the minimum window. Without it the track measured its
        // labels' natural widths, which are narrower than the floors of short labels
        // ("Full"), and the last segment wrapped under the others in a wide window.
        let floor = options
            .iter()
            .map(|(label, _)| components::segment_reserved_width(label, metrics))
            .fold(px(0.0), |sum, width| sum + width)
            + metrics.scale(spacing::HALF) * options.len().saturating_sub(1) as f32
            + metrics.scale(SEGMENTED_FLOOR_PADDING) * 2.0
            + px(components::HAIRLINE_WIDTH * 2.0);
        let mut control = components::segmented(metrics)
            .min_w(floor.min(components::settings_row_content_cap(metrics)))
            .debug_selector(move || id.to_owned());
        for (index, (label, selected)) in options.into_iter().enumerate() {
            let pick = pick.clone();
            let reserved = components::segment_reserved_width(&label, metrics);
            control = control.child(
                components::segment(label, selected, metrics)
                    .min_w(reserved)
                    .max_w_full()
                    .flex_shrink()
                    .whitespace_normal()
                    .h(gpui::auto())
                    .min_h(metrics.compact_control_height())
                    .id(SharedString::from(format!("{id}.{index}")))
                    .debug_selector(move || format!("{id}.{index}"))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        pick(this, index, cx);
                    })),
            );
        }
        control
    }

    /// A setting that is on or off, as a switch with its state word beside it (R5).
    ///
    /// The whole of it is the pointer target - at least a finger high - and a press
    /// flips the state and calls `set` with the new one. The word says the state in
    /// text as well, so it is not told by the knob's side and colour alone.
    pub(super) fn switch_control(
        &self,
        id: &'static str,
        on: bool,
        cx: &mut Context<Self>,
        set: impl Fn(&mut Self, bool, &mut Context<Self>) + 'static,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        components::labelled_switch(
            on,
            if on { ux.notice_on } else { ux.notice_off },
            id,
            metrics,
        )
        .id(id)
        .debug_selector(move || id.to_owned())
        .on_click(cx.listener(move |this, _event, _window, cx| {
            set(this, !on, cx);
        }))
    }

    /// Sets the theme and repaints the terminals in it.
    pub(super) fn set_theme(&mut self, choice: ThemeChoice, cx: &mut Context<Self>) {
        self.theme_choice = choice;
        self.repaint_terminals(cx);
        cx.notify();
    }

    /// Sets the terminal font size for new terminals and every open one.
    pub(super) fn set_terminal_font_size(&mut self, size: f32, cx: &mut Context<Self>) {
        self.workspace.set_local_font_size(size);
        let size = self.workspace.local_font_size();
        let panes: Vec<_> = self
            .panes
            .iter()
            .filter_map(|open| open.content.terminal().cloned())
            .collect();
        for pane in panes {
            pane.update(cx, |pane, cx| pane.set_font_size(size, cx));
        }
        cx.notify();
    }

    /// Switches one background reminder on or off (R7).
    ///
    /// The choice is written with the other preferences on the next draw, so it
    /// survives a restart. Turning a reminder off drops the marks it already made and
    /// touches nothing else: no bell byte is eaten, no block is rewritten and no reason
    /// for a disconnect is hidden.
    pub(super) fn set_attention_notice(
        &mut self,
        kind: AttentionKind,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.set_attention(kind, on) && self.persist_preferences() {
            self.note_data_changed(cx);
        }
        cx.notify();
    }

    /// Switches one kind of desktop notification on or off (R2). Saved with the
    /// other preferences, on this computer only.
    pub(super) fn set_desktop_notify(
        &mut self,
        kind: NotifyKind,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if self.workspace.set_notify(kind, on) && self.persist_preferences() {
            self.note_data_changed(cx);
        }
        cx.notify();
    }

    /// Sets how long a command must run for its end to be a desktop notification
    /// (R2), within the allowed range.
    pub(super) fn set_long_command_secs(&mut self, secs: u32, cx: &mut Context<Self>) {
        if self.workspace.set_long_command_secs(secs) && self.persist_preferences() {
            self.note_data_changed(cx);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    /// A centred dialog over a dimmed window, laid in the window's overlay layer.
    ///
    /// The dialog is [`components::modal`]; a press on the dimmed part does not close
    /// it, so what was typed is kept until the user closes it on purpose.
    pub(super) fn modal(
        &self,
        window: &Window,
        id: &'static str,
        width: f32,
        content: ModalContent,
    ) -> Div {
        self.modal_drawn.set(true);
        super::overlay_layer().child(components::modal(
            window.viewport_size(),
            id,
            width,
            content,
            self.metrics(),
        ))
    }

    /// [`Self::modal`] with its own scroll handle or its own answer to a press outside.
    pub(super) fn modal_with(
        &self,
        window: &Window,
        id: &'static str,
        width: f32,
        content: ModalContent,
        behaviour: ModalBehaviour,
    ) -> Div {
        self.modal_drawn.set(true);
        super::overlay_layer().child(components::modal_with(
            window.viewport_size(),
            id,
            width,
            content,
            behaviour,
            self.metrics(),
        ))
    }

    /// The header of a dialog: an optional icon tile, a title, a subtitle and a
    /// close button.
    pub(super) fn modal_header(
        &self,
        icon: Option<Icon>,
        title: String,
        subtitle: Option<String>,
        close_id: &'static str,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> (Div, Option<Div>) {
        self.modal_header_with(icon, title, subtitle, None, close_id, cx, close)
    }

    /// [`Self::modal_header`] with a badge under the title.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn modal_header_with(
        &self,
        icon: Option<Icon>,
        title: String,
        subtitle: Option<String>,
        badge: Option<Div>,
        close_id: &'static str,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> (Div, Option<Div>) {
        let metrics = self.metrics();
        let lead = icon.map(|icon| components::modal_icon(icon, None, metrics));
        self.modal_header_lead(lead, title, subtitle, badge, close_id, cx, close)
    }

    /// The header of a dialog with any leading tile: a host's avatar, or an icon.
    ///
    /// The title and the badge share one column beside the tile. The subtitle is
    /// returned separately and drawn at the top of the scrolling body: beside the
    /// icon it wrapped one glyph per line, and in the fixed header a full sentence
    /// at 200% text left the body 24px tall, so the editor could not be scrolled
    /// into view (R4). A title with a badge wraps on spaces: at 720×480 and 200% the
    /// column is narrower than one word, and truncate would cut a glyph with no
    /// ellipsis.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn modal_header_lead(
        &self,
        lead: Option<Div>,
        title: String,
        subtitle: Option<String>,
        badge: Option<Div>,
        close_id: &'static str,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> (Div, Option<Div>) {
        let metrics = self.metrics();
        let wrapping = badge.is_some();
        // A dialog's title is the title size (16), not a page heading: the dialog is
        // a question over the page, and its words are what matter (ADR-1, ADR-7).
        let title = div()
            .min_w_0()
            .text_size(metrics.title_size())
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::swatch(palette::TEXT_PRIMARY))
            // Whole words, wrapped: a one-line truncate inside the flexible column
            // could collapse a short title to a bare ellipsis.
            .w_full()
            .whitespace_normal()
            .child(SharedString::from(title));
        let column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(metrics.scale(spacing::HAIRLINE))
            .child(title)
            .children(badge.map(|badge| div().flex().min_w_0().child(badge)));
        let bar = div()
            .flex()
            .flex_none()
            .w_full()
            .min_w_0()
            .when(wrapping, |bar| bar.items_start())
            .when(!wrapping, |bar| bar.items_center())
            .gap(metrics.scale(spacing::SNUG))
            .pl(metrics.scale(spacing::LOOSE))
            .pr(metrics.scale(spacing::BASE))
            .pt(metrics.scale(spacing::BASE))
            .pb(metrics.scale(spacing::TIGHT))
            .children(lead)
            .child(column)
            .child(
                components::icon_only_button(Icon::Close, metrics)
                    .id(close_id)
                    .debug_selector(move || close_id.to_owned())
                    .tooltip(components::tooltip(copy::ux().tool_close))
                    .on_click(cx.listener(move |this, _event, _window, cx| {
                        close(this, cx);
                    })),
            );
        let subtitle = subtitle.map(|subtitle| {
            div()
                // Whole words across the dialog. A single-line truncate cut the
                // sentence through a glyph and drew no ellipsis (R4).
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .px(metrics.scale(spacing::LOOSE))
                .pb(metrics.scale(spacing::TIGHT))
                .text_size(metrics.label_size())
                .text_color(theme::swatch(palette::TEXT_SECONDARY))
                .debug_selector(|| "modal.subtitle".to_owned())
                .child(SharedString::from(subtitle))
        });
        (bar, subtitle)
    }

    /// Where a running attempt is: four steps, the one in flight marked, and the way
    /// it signs in written once underneath (R17).
    ///
    /// It replaces a stage line, a target line, a status chip and a form that all said
    /// "connecting" at once. Nothing here is editable: an attempt in flight cannot change
    /// how it signs in, and fields that looked editable only invited a change that would
    /// not take effect. When the server or the shell asks something, the step that
    /// asks is marked as waiting for the user and the question follows below.
    fn connection_progress(&self) -> Div {
        use crate::app::remote::RemoteStatus;
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let Some(panel) = self.remote.as_ref() else {
            return div();
        };
        let host = panel.host();
        let status = panel.status();
        let session_state = self
            .form_session
            .and_then(|id| self.workspace.session(id))
            .map(|session| session.state().clone());
        let (current, waiting) = match status {
            RemoteStatus::VerifyingHost(_) => (1, Some(strings.remote_wait_identity)),
            RemoteStatus::ConfirmingProxy { .. } => (0, Some(strings.remote_proxy_title)),
            RemoteStatus::ConfirmingX11 { .. } => (0, Some(strings.x11_trusted_title)),
            _ => match session_state {
                Some(SessionState::VerifyingHost) => (1, None),
                Some(SessionState::Authenticating) => (2, None),
                _ => (0, None),
            },
        };
        let steps = [
            ux.connect_step_dial,
            ux.connect_step_identity,
            ux.connect_step_login,
            ux.connect_step_terminal,
        ];
        let mut list = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT + 2.0))
            .py(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::BASE))
            .rounded(px(radius::MEDIUM))
            .bg(theme::swatch(palette::CHROME))
            .border_1()
            .border_color(theme::swatch(palette::BORDER))
            .debug_selector(|| "remote.stage".to_owned());
        for (index, label) in steps.iter().enumerate() {
            let done = index < current;
            let now = index == current;
            let mark = if done {
                icons::icon(
                    Icon::Check,
                    metrics.small_icon_size(),
                    theme::swatch(palette::POSITIVE),
                )
                .into_any_element()
            } else if now && waiting.is_some() {
                components::status_mark(
                    Tone::Warning,
                    metrics.scale(components::ATTENTION_MARK_SIZE),
                )
                .into_any_element()
            } else if now {
                progress_pulse(metrics)
            } else {
                div()
                    .size(metrics.scale(STEP_DOT_SIZE))
                    .rounded_full()
                    .bg(theme::swatch(palette::BORDER_STRONG))
                    .into_any_element()
            };
            let mut line = div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(metrics.scale(spacing::TIGHT))
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(metrics.icon_size())
                        .child(mark),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(metrics.label_size())
                        .when(now, |text| text.font_weight(FontWeight::MEDIUM))
                        .text_color(theme::swatch(if now {
                            palette::TEXT_PRIMARY
                        } else if done {
                            palette::TEXT_SECONDARY
                        } else {
                            palette::TEXT_MUTED
                        }))
                        .when(now, |text| {
                            text.debug_selector(|| "remote.stage.value".to_owned())
                        })
                        .child(*label),
                );
            // The first step says where it is going, the third with what it signs in:
            // the two facts a person checks when a connection takes long.
            if index == 0 {
                line = line.child(
                    components::copy_on_click(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(crate::app::terminal::monospace_family())
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_MUTED))
                            .debug_selector(|| "remote.target".to_owned()),
                        host.target(),
                    )
                    .child(SharedString::from(host.target())),
                );
            }
            if index == 2 {
                let method = panel.method();
                let detail = if method.needs_key_path() && !panel.key_path().is_empty() {
                    format!("{} · {}", method.label(strings), panel.key_path())
                } else {
                    method.label(strings).to_owned()
                };
                line = line.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "remote.method.summary".to_owned())
                        .child(SharedString::from(detail)),
                );
            }
            if now && let Some(detail) = waiting {
                line = line
                    .child(
                        components::chip(strings.remote_waiting, Tone::Warning, metrics)
                            .debug_selector(|| "remote.waiting".to_owned()),
                    )
                    .child(
                        div()
                            .text_size(metrics.caption_size())
                            .text_color(theme::swatch(palette::TEXT_SECONDARY))
                            .debug_selector(|| "remote.waiting.detail".to_owned())
                            .child(SharedString::from(detail)),
                    );
            }
            list = list.child(line);
        }
        list
    }

    /// The connection being set up, or the quick-connect form, as a dialog.
    pub(super) fn connection_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        if self.quick_open {
            let (header, subtitle) = self.modal_header(
                Some(Icon::Plug),
                strings.quick_connect_title.to_owned(),
                Some(ux.quick_hint.to_owned()),
                "quick.close",
                cx,
                |this, cx| {
                    this.quick_open = false;
                    this.quick_error = None;
                    this.restore_focus_pending = true;
                    cx.notify();
                },
            );
            let body = div()
                .px(metrics.scale(spacing::LOOSE))
                .pb(metrics.scale(spacing::LOOSE))
                .child(self.quick_panel(cx))
                .into_any_element();
            return Some(self.modal(
                window,
                "quick.dialog",
                520.0,
                ModalContent {
                    header,
                    subtitle,
                    body,
                    footer: None,
                },
            ));
        }
        if !(self.showing_connection_form && self.remote.is_some()) {
            return None;
        }
        // An attempt that has its own tab is shown in that tab, not over the page.
        // A reconnect that asks nothing runs under the banner over the last screen it
        // keeps (ADR-6); its dialog comes back when it asks a question or fails.
        if self.connection_inline() || self.connection_behind_screen() {
            return None;
        }
        let content = self.connection_content(cx)?;
        let entrance = self
            .remote
            .as_ref()
            .filter(|panel| matches!(panel.status(), crate::app::remote::RemoteStatus::Connecting))
            .map(|_| theme::Enter::Rise);
        Some(self.modal_with(
            window,
            "remote.dialog",
            600.0,
            content,
            ModalBehaviour {
                entrance,
                ..Default::default()
            },
        ))
    }

    /// Whether the connection being set up is drawn in its own tab rather than as a
    /// dialog: it has a session, and that session's tab is the one on screen.
    ///
    /// Pressing Connect is going to that server, so the window goes there: the tab
    /// opens at once and the steps, a question or a failure are where the terminal is
    /// about to be, instead of a dialog over the list that was just left.
    pub(super) fn connection_inline(&self) -> bool {
        self.showing_connection_form
            && !self.quick_open
            && self.remote.is_some()
            && self.workspace.destination() == Destination::LocalShells
            && self
                .form_session
                .is_some_and(|id| self.active == Some(id) && self.workspace.session(id).is_some())
    }

    /// The connection being set up, drawn in the terminal area of its tab.
    pub(super) fn connection_page(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let metrics = self.metrics();
        let ModalContent {
            header,
            subtitle,
            body,
            footer,
        } = self.connection_content(cx)?;
        Some(
            div()
                .id("remote.page")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .items_center()
                .overflow_y_scroll()
                .bg(theme::swatch(palette::CANVAS))
                .px(metrics.scale(spacing::LOOSE))
                .py(metrics.scale(spacing::LOOSE * 2.0))
                .debug_selector(|| "remote.dialog".to_owned())
                // The attempt's card is the status card of a session on its way: it
                // rises into place in the tab, like the card an ended session shows (R3).
                .child(theme::enter(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        // A definite width, capped by the page: a card only as
                        // wide as "full, at most N" was measured before its footer's
                        // buttons wrapped, and they hung below its edge.
                        .w(metrics.scale(CONNECTION_CARD_MAX_WIDTH))
                        .max_w_full()
                        .rounded(px(radius::XLARGE))
                        .bg(theme::swatch(palette::SURFACE))
                        .border_1()
                        .border_color(theme::swatch(palette::BORDER))
                        .shadow(theme::Elevation::Overlay.shadows())
                        .debug_selector(|| "remote.page.card".to_owned())
                        .child(header)
                        .children(subtitle)
                        .child(body)
                        .children(footer),
                    "remote.page.enter",
                    theme::Enter::Rise,
                )),
        )
    }

    /// What the connection dialog holds - head, body and buttons - wherever it is drawn.
    fn connection_content(&self, cx: &mut Context<Self>) -> Option<ModalContent> {
        let metrics = self.metrics();
        let strings = self.strings();
        let ux = copy::ux();
        let panel = self.remote.as_ref()?;
        if matches!(panel.status(), crate::app::remote::RemoteStatus::Connecting)
            && self
                .form_session
                .and_then(|id| self.workspace.connection_wait(id))
                .is_some_and(|wait| !wait.due(cx))
        {
            return None;
        }
        let host = panel.host();
        let (status_label, tone) = super::remote_status_label(panel.status(), strings);
        // The hint is about choosing a sign-in; while the server asks a question or
        // the connection runs, it would only be in the way.
        // A failure already says what happened and what to do next. The sign-in hint
        // on top of it pushed that next line past the window (R4).
        let choosing = matches!(panel.status(), crate::app::remote::RemoteStatus::Idle);
        let failed = matches!(panel.status(), crate::app::remote::RemoteStatus::Failed(_));
        // The head names the host and the address once. The state is the body's job:
        // a chip here said "connecting" a second time beside the progress that says it.
        let in_flight = panel.status().is_busy() || panel.status().is_asking();
        let (header, subtitle) = self.modal_header_lead(
            Some(host_avatar(
                host.name(),
                &self
                    .monograms()
                    .remove(&host.id())
                    .unwrap_or_else(|| crate::app::monogram::initials(host.name())),
                self.host_platform(host.id()),
                false,
                metrics,
            )),
            format!("{} {}", ux.connect_title, host.name()),
            // While an attempt runs, its first step names the address; saying it under
            // the title too was the same line twice.
            (!in_flight).then(|| host.target()),
            None,
            "remote.close",
            cx,
            |this, cx| this.close_remote(cx),
        );
        let asking = panel.status().is_asking();
        // The progress belongs to an attempt in flight or waiting for an answer. A form
        // that has not started and a finished failure say what they are on their own.
        let show_progress = panel.status().is_busy() || asking;
        let _ = (status_label, tone);
        let body = div()
            .flex()
            .flex_col()
            .w_full()
            .min_w_0()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .when(show_progress, |body| body.child(self.connection_progress()))
            .when(choosing, |body| {
                body.child(
                    div()
                        .text_size(metrics.label_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(ux.connect_hint),
                )
            })
            .child(self.remote_panel(cx))
            .child(self.other_attempts(cx))
            .into_any_element();
        // A connection that is already running can go on without the dialog: it keeps
        // its tab, and clicking that tab brings the dialog back. A failure is finished,
        // so that button would only take the room the reason and the next step need.
        // An attempt that is really in flight can also be ended here, which is the
        // only place it can be ended while the dialog is still what is on screen.
        let can_background = self.form_session.is_some() && in_flight && !failed;
        let can_cancel = panel.status().is_busy() || asking;
        let footer = (can_background || can_cancel).then(|| {
            components::modal_footer(metrics)
                .when(can_cancel, |row| {
                    row.child(self.text_button(
                        SharedString::from("remote.cancel"),
                        strings.remote_cancel_attempt,
                        Variant::Secondary,
                        cx,
                        |view, cx| view.close_remote(cx),
                    ))
                })
                .when(can_background, |row| {
                    row.child(self.text_button(
                        SharedString::from("remote.hide"),
                        ux.connect_background,
                        Variant::Ghost,
                        cx,
                        |view, cx| view.hide_connection(cx),
                    ))
                })
        });
        Some(ModalContent {
            header,
            subtitle,
            body,
            footer,
        })
    }

    /// Importing hosts from an SSH configuration, as a dialog: read, preview, then
    /// save only what the preview offered.
    pub(super) fn import_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if !self.import.is_open() {
            return None;
        }
        let metrics = self.metrics();
        let strings = self.strings();
        let (header, subtitle) = self.modal_header(
            Some(Icon::Import),
            strings.import_title.to_owned(),
            Some(strings.import_read_only.to_owned()),
            "hosts.import.dialog.close",
            cx,
            |this, cx| this.close_import(cx),
        );
        let body = div()
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .child(self.import_panel(cx))
            .into_any_element();
        let footer = self.import_actions(cx);
        Some(self.modal(
            window,
            "hosts.import.dialog",
            720.0,
            ModalContent {
                header,
                subtitle,
                body,
                footer: Some(footer),
            },
        ))
    }

    /// Starting any program by its path: a field, one line on what it takes, and
    /// the reason when the last try did not start.
    pub(super) fn custom_shell_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.custom_shell_open {
            return None;
        }
        let metrics = self.metrics();
        let ux = copy::ux();
        let (header, subtitle) = self.modal_header(
            Some(Icon::FolderOpen),
            ux.custom_shell_title.to_owned(),
            None,
            "shell.custom.close",
            cx,
            |this, cx| {
                this.custom_shell_open = false;
                this.restore_focus_pending = true;
                cx.notify();
            },
        );
        let body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::LOOSE))
            .child(
                div()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(ux.custom_shell_body),
            )
            .child(
                div()
                    .debug_selector(|| "shell.path".to_owned())
                    // Escape closes the dialog; without this the field would take it
                    // to empty itself, and the path just typed would be lost.
                    .capture_key_down(cx.listener(
                        |this, event: &gpui::KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                cx.stop_propagation();
                                this.close_custom_shell(window, cx);
                            }
                        },
                    ))
                    .child(self.shell_path.clone()),
            )
            .children(
                self.local_error
                    .as_ref()
                    .map(|reason| self.notice_line(reason.clone(), Tone::Danger)),
            )
            .into_any_element();
        let can_run = !self.shell_path.read(cx).text().trim().is_empty();
        let footer = components::modal_footer(metrics)
            .child(self.text_button(
                SharedString::from("shell.custom.cancel"),
                ux.fwd_cancel,
                Variant::Ghost,
                cx,
                |this, cx| {
                    this.custom_shell_open = false;
                    this.restore_focus_pending = true;
                    cx.notify();
                },
            ))
            .child(
                components::button(ux.custom_shell_run, Variant::Primary, metrics)
                    .id("shell.custom.run")
                    .debug_selector(|| "shell.custom.run".to_owned())
                    .when(!can_run, |button| button.opacity(0.5))
                    .when(can_run, |button| {
                        button.on_click(cx.listener(|this, _event, _window, cx| {
                            let text = this.shell_path.read(cx).text().to_owned();
                            this.start_custom_shell(text, cx);
                        }))
                    }),
            );
        Some(self.modal(
            window,
            "shell.custom.dialog",
            520.0,
            ModalContent {
                header,
                subtitle,
                body,
                footer: Some(footer),
            },
        ))
    }

    /// Port forwarding or transfers, opened from a host or a session.
    pub(super) fn tool_overlay(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        let tool = self.tool?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.tool = None;
            this.remember_tool_focus_pending = false;
            this.forwards.close_editor();
            this.pending_exposure = None;
            // A question about one rule is about a page that is no longer on screen.
            this.forward_confirm = None;
            this.snippet_panel.keep();
            this.snippet_panel.end_edit();
            this.restore_focus_pending = true;
            cx.notify();
        };
        match tool {
            Tool::Forwards => {
                let subtitle = match self.forwards.scope() {
                    crate::app::forwarding::ForwardScope::All => ux.tool_forwards_all.to_owned(),
                    crate::app::forwarding::ForwardScope::Host(id) => format!(
                        "{} {}",
                        ux.tool_forwards_host,
                        self.book.host(id).map_or("?", |host| host.name())
                    ),
                };
                let (header, hint) = self.modal_header(
                    Some(Icon::Forward),
                    ux.tool_forwards_title.to_owned(),
                    Some(subtitle),
                    "forwards.dialog.close",
                    cx,
                    close,
                );
                let body = div()
                    .flex()
                    .flex_col()
                    .gap(metrics.scale(spacing::SNUG))
                    .px(metrics.scale(spacing::LOOSE))
                    .pb(metrics.scale(spacing::LOOSE))
                    .child(self.forward_rows(
                        div().flex().flex_col().gap(metrics.scale(spacing::TIGHT)),
                        cx,
                    ))
                    .into_any_element();
                Some(self.modal(
                    window,
                    "forwards.dialog",
                    680.0,
                    ModalContent {
                        header,
                        subtitle: hint,
                        body,
                        footer: None,
                    },
                ))
            }
            Tool::Transfers => {
                let (header, subtitle) = self.modal_header(
                    Some(Icon::Transfer),
                    ux.tool_transfers_title.to_owned(),
                    Some(ux.tool_transfers_subtitle.to_owned()),
                    "transfers.dialog.close",
                    cx,
                    close,
                );
                let state = self.workspace.state(Destination::Transfers);
                let list = match state {
                    SectionState::Populated => {
                        let card = self.section_card(Destination::Transfers, state);
                        self.rows_for(Destination::Transfers, card, cx)
                    }
                    // Nothing to list is a state of its own with the shared empty look:
                    // what the panel holds and where a transfer starts (ADR-7).
                    SectionState::Empty => components::empty_state(
                        Icon::Transfer,
                        self.strings().empty_transfers_title,
                        Some(SharedString::from(self.strings().empty_transfers_body)),
                        None,
                        metrics,
                    )
                    .debug_selector(|| "transfers.empty".to_owned()),
                    _ => self.section_card(Destination::Transfers, state),
                };
                let body = div()
                    .px(metrics.scale(spacing::LOOSE))
                    .pb(metrics.scale(spacing::LOOSE))
                    .child(list)
                    .into_any_element();
                let footer = self.transfers.has_finished().then(|| {
                    components::modal_footer(metrics)
                        .justify_end()
                        .child(self.text_button(
                            SharedString::from("transfers.center.clear_finished"),
                            ux.transfers_clear_finished,
                            Variant::Secondary,
                            cx,
                            |this, cx| this.clear_finished_transfers(None, cx),
                        ))
                });
                Some(self.modal(
                    window,
                    "transfers.dialog",
                    680.0,
                    ModalContent {
                        header,
                        subtitle,
                        body,
                        footer,
                    },
                ))
            }
            Tool::Snippets => Some(self.snippet_tool(window, cx, close)),
        }
    }

    /// Puts the connection dialog away while its attempt keeps running in its tab.
    pub(super) fn hide_connection(&mut self, cx: &mut Context<Self>) {
        self.security_reset_reveal();
        let hidden = self.form_session;
        self.park_focused_form();
        // The form now lives on its attempt; nothing is on screen until its tab asks
        // for it again.
        self.form_session = None;
        self.showing_connection_form = false;
        // When the attempt was started inside a multi-tab terminal workspace, step back
        // to another live workspace tab so the user stays in their terminals; otherwise
        // return to the hosts list while the attempt keeps running.
        if self.active.is_some_and(|id| {
            self.pane_for(id).is_none()
                && self.desktop_for(id).is_none()
                && self.attempt_index(id).is_some()
        }) {
            let fallback = self
                .remote_opened_from_workspace
                .then(|| {
                    self.tab_sessions().into_iter().find(|candidate| {
                        Some(*candidate) != hidden
                            && (self.pane_for(*candidate).is_some()
                                || self.desktop_for(*candidate).is_some())
                    })
                })
                .flatten();
            if let Some(other) = fallback {
                self.activate(other, cx);
                return;
            }
            self.workspace.select(Destination::Hosts);
        }
        cx.notify();
    }

    /// Opens a tool over the current page.
    pub(super) fn open_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        if self.batch.view().is_open() {
            self.retire_batch_focus(cx);
            let _ = self.batch.close() || self.batch.hide();
        }
        self.close_import_for_replacement(cx);
        self.close_editors_for_replacement(cx);
        if self.tool != Some(Tool::Snippets) || tool != Tool::Snippets {
            self.snippet_panel.keep();
            self.snippet_panel.end_edit();
        }
        self.remember_tool_focus_pending = true;
        self.tool = Some(tool);
        self.shell_menu_open = false;
        cx.notify();
    }

    /// The page shown while no terminal is open on the terminals destination.
    #[allow(dead_code)]
    pub(super) fn terminals_empty(&self) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        components::state_card_with_icon(
            Tone::Neutral,
            Some(Icon::Terminal),
            ux.terminals_empty_title,
            ux.terminals_empty_body,
            None,
            metrics,
        )
    }

    /// Whether the hosts home fills the content area, for a test.
    #[cfg(test)]
    pub fn home_is_shown_for_test(&self) -> bool {
        self.home_is_shown()
    }

    /// The tool open over the page, for a test.
    #[cfg(test)]
    pub fn tool_for_test(&self) -> Option<Tool> {
        self.tool
    }

    /// Whether the inline connection form is currently rendered in the active tab, for a test.
    #[cfg(test)]
    pub fn connection_inline_for_test(&self) -> bool {
        self.connection_inline()
    }
}

// ---------------------------------------------------------------------------
// Shortcut commands (R6)
// ---------------------------------------------------------------------------

impl ShelluneView {
    /// Opens the manager of the saved commands, on the list or on a new command.
    ///
    /// Opening it reads nothing new: the library was read when the window started, and a
    /// file that could not be read then is still reported here. Nothing is inserted into a
    /// terminal by opening the manager.
    pub(crate) fn open_snippets(&mut self, new: bool, cx: &mut Context<Self>) {
        self.open_tool(Tool::Snippets, cx);
        self.snippet_panel.keep();
        if self.snippet_panel.search_field().is_none() {
            let placeholder = copy::ux_for(self.workspace.locale()).snippet_search_placeholder;
            let field = cx.new(|cx| {
                let mut field = TextField::new(placeholder, cx);
                field.set_leading_icon(Icon::Search, cx);
                field.set_clearable(true, cx);
                field
            });
            cx.observe(&field, |view, field, cx| {
                let text = field.read(cx).text().to_owned();
                if view.snippet_panel.set_query(text) {
                    cx.notify();
                }
            })
            .detach();
            self.snippet_panel.set_search_field(field);
        } else if let Some(field) = self.snippet_panel.search_field().cloned() {
            field.update(cx, |field, cx| field.set_text("", cx));
            self.snippet_panel.set_query("");
        }
        if new {
            self.open_snippet_editor(None, cx);
        } else {
            self.close_snippet_editor(cx);
            // When saved commands exist, the search field takes focus so typing filters
            // immediately while Up/Down/Enter walk and insert; when empty, the dialog
            // body takes focus so Escape and keyboard navigation still hold it.
            self.snippet_focus_pending = Some(self.preferred_snippet_list_focus(cx));
        }
        cx.notify();
    }

    pub(super) fn preferred_snippet_list_focus(&self, cx: &gpui::App) -> gpui::FocusHandle {
        if !self.snippets.snippets().is_empty()
            && let Some(field) = self.snippet_panel.search_field()
        {
            field.read(cx).focus_handle().clone()
        } else {
            self.snippet_focus.clone()
        }
    }

    /// Opens the editor on one command, or on a command that has not been written yet.
    ///
    /// The fields are made per opening, so they start from what is really saved (or are
    /// empty for a new one) and their placeholders are in the language on screen. The name
    /// field takes the keyboard on the next draw, which is where a dialog's first field
    /// belongs.
    fn open_snippet_editor(&mut self, id: Option<SnippetId>, cx: &mut Context<Self>) {
        let ux = copy::ux();
        let (name_text, body_text) = match id.and_then(|id| self.snippets.get(id).cloned()) {
            Some(snippet) => (snippet.name().to_owned(), snippet.body().to_owned()),
            None => (String::new(), String::new()),
        };
        let name = cx.new(|cx| TextField::with_text(ux.snippet_name_placeholder, &name_text, cx));
        let body = cx.new(|cx| {
            let mut field = TextField::with_text(ux.snippet_body_placeholder, &body_text, cx);
            // A command may be a short block, so the body keeps its line breaks; Enter
            // still saves, which is what keeps the dialog on one keyboard path.
            field.set_keeps_line_breaks(true, cx);
            field
        });
        // Enter in either field saves what is typed. The field keeps its text so a refused
        // draft stays on screen beside the reason it was refused.
        let view = cx.entity().downgrade();
        for field in [name.clone(), body.clone()] {
            field.update(cx, |field, _cx| {
                let view = view.clone();
                field.on_submit(move |_text, _window, app| {
                    view.update(app, |view, cx| {
                        view.save_snippet_editor(cx);
                        false
                    })
                    .unwrap_or(false)
                });
            });
            // The dialog redraws as the draft changes, so the length warning appears while
            // the text can still be shortened rather than only after a refused save.
            cx.observe(&field, |_view, _field, cx| cx.notify()).detach();
        }
        TextField::wire_tab_ring(&[name.clone(), body.clone()], cx);
        let handle = name.read(cx).focus_handle().clone();
        self.snippet_panel.begin_edit(SnippetEditor {
            id,
            name,
            body,
            problem: None,
        });
        // The keyboard goes to the first field on the next draw, which is the only place
        // with a window to give it to.
        self.snippet_focus_pending = Some(handle);
        cx.notify();
    }

    /// Puts the editor away without saving anything. What was typed is dropped.
    pub(super) fn close_snippet_editor(&mut self, cx: &mut Context<Self>) {
        if self.snippet_panel.end_edit().is_some() {
            self.snippet_list_focus(cx);
            cx.notify();
        }
    }

    /// Gives the manager's list (or search box) the keyboard again once the editor is gone.
    fn snippet_list_focus(&mut self, cx: &gpui::App) {
        if self.tool == Some(Tool::Snippets) {
            self.snippet_focus_pending = Some(self.preferred_snippet_list_focus(cx));
        }
    }

    /// Saves what the editor holds, or says why it cannot be saved.
    ///
    /// The library writes the file first and adopts the change only once the file has it,
    /// so a failed save leaves the list as it was and the draft on screen.
    fn save_snippet_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.snippet_panel.editor() else {
            return;
        };
        let id = editor.id;
        let name_field = editor.name.clone();
        let body_field = editor.body.clone();
        let name = name_field.read(cx).text().to_owned();
        let body = body_field.read(cx).text().to_owned();
        let locale = self.workspace.locale();
        let outcome = match id {
            Some(id) => self.snippets.replace(id, &name, &body),
            None => self.snippets.create(&name, &body).map(|_id| ()),
        };
        match outcome {
            Ok(()) => {
                self.snippet_panel.end_edit();
                self.snippet_list_focus(cx);
                self.note_data_changed(cx);
                self.toast(Tone::Positive, copy::ux().snippet_saved, None, cx);
            }
            Err(error) => {
                let reason = error.text(locale);
                if let Some(editor) = self.snippet_panel.editor_mut() {
                    editor.problem = Some(reason);
                }
            }
        }
        cx.notify();
    }

    /// Deletes the command the question was asked about.
    fn confirm_snippet_delete(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.snippet_panel.pending_delete() else {
            return;
        };
        self.snippet_panel.keep();
        let locale = self.workspace.locale();
        match self.snippets.remove(id) {
            Ok(true) => {
                self.note_data_changed(cx);
                let count = self.snippets.snippets().len();
                self.snippet_panel
                    .select(self.snippet_panel.selected(), count);
                self.snippet_list_focus(cx);
                self.toast(Tone::Positive, copy::ux().snippet_deleted, None, cx);
            }
            Ok(false) => {}
            Err(error) => {
                // The command is still in the library, because the file still holds it.
                self.toast(Tone::Danger, error.text(locale), None, cx);
            }
        }
        cx.notify();
    }

    /// Offers one saved command to the terminal on screen (R6).
    ///
    /// Inserting types the body and submits nothing; running types it and sends one line
    /// break. Both go through the pane's own paste rule, so a multi-line or high-risk body
    /// is confirmed first and cancelling writes nothing. Without a terminal on screen
    /// nothing is created and nothing is reported as run: the window says what to open
    /// first.
    ///
    /// What the corner says follows what the pane actually did. A body that reached the
    /// pane is reported as inserted or sent; a body held behind the paste prompt is not,
    /// because nothing has been written yet and cancelling writes nothing — the prompt in
    /// the pane is the only place that completion can come from.
    pub(crate) fn use_snippet(&mut self, id: SnippetId, execute: bool, cx: &mut Context<Self>) {
        let ux = copy::ux();
        let Some(body) = self.snippets.get(id).map(|s| s.body().to_owned()) else {
            self.toast(
                Tone::Danger,
                ux.snippet_problem_unknown.to_owned(),
                None,
                cx,
            );
            return;
        };
        let pane = self
            .shows_terminal(self.workspace.destination())
            .then(|| self.active_pane().cloned())
            .flatten();
        let Some(pane) = pane else {
            self.toast(
                Tone::Warning,
                ux.snippet_no_terminal.to_owned(),
                Some(ux.snippet_no_terminal_body.to_owned()),
                cx,
            );
            return;
        };
        // A pane whose shell has ended keeps its last screen; typing into it would go
        // nowhere, so nothing is reported as inserted or run.
        if !pane.read(cx).state().accepts_input() {
            self.toast(
                Tone::Warning,
                ux.snippet_terminal_ended.to_owned(),
                Some(ux.snippet_terminal_ended_body.to_owned()),
                cx,
            );
            return;
        }
        let offer = pane.update(cx, |pane, cx| pane.stage_snippet(&body, execute, cx));
        match offer {
            SnippetOffer::Written => {
                if self.tool == Some(Tool::Snippets) {
                    self.tool = None;
                    self.remember_tool_focus_pending = false;
                    self.snippet_panel.keep();
                    self.snippet_panel.end_edit();
                    self.focus_return = None;
                    self.pane_focus_pending = self.active;
                }
                self.settle_snippet_note(cx);
                let title = if execute {
                    ux.snippet_sent
                } else {
                    ux.snippet_inserted
                };
                self.toast(Tone::Positive, title, None, cx);
                cx.notify();
            }
            SnippetOffer::Waiting => {
                if self.tool == Some(Tool::Snippets) {
                    self.tool = None;
                    self.remember_tool_focus_pending = false;
                    self.snippet_panel.keep();
                    self.snippet_panel.end_edit();
                }
                // Nothing has been written: the corner points at the prompt instead of
                // reporting a command that did not go anywhere yet, and the note leaves
                // when the prompt is answered.
                self.settle_snippet_note(cx);
                let toast = self.toast(
                    Tone::Neutral,
                    ux.snippet_waiting.to_owned(),
                    Some(copy::fill(
                        ux.snippet_waiting_body,
                        &[(
                            "cancel",
                            self.workspace.locale().strings().terminal_paste_cancel,
                        )],
                    )),
                    cx,
                );
                if let Some(session) = self.active {
                    self.snippet_note = Some(SnippetNote { session, toast });
                }
                cx.notify();
            }
            // A stored body with nothing in it cannot type anything, so nothing is claimed.
            SnippetOffer::Empty => {
                self.settle_snippet_note(cx);
                self.toast(
                    Tone::Warning,
                    ux.snippet_problem_body_empty.to_owned(),
                    None,
                    cx,
                );
            }
        }
    }

    /// Takes the "waiting for confirmation" note away, if one is up.
    fn settle_snippet_note(&mut self, cx: &mut Context<Self>) {
        if let Some(note) = self.snippet_note.take() {
            self.dismiss_toast(note.toast, cx);
        }
    }

    /// Takes the note away as soon as the prompt it describes has been answered.
    ///
    /// The answer is given in the terminal — confirmed or cancelled — and the terminal is
    /// what holds the command, so the note leaves the corner with the prompt. What is left
    /// on screen is then the plain fact: a confirmed command is in the pane, and a
    /// cancelled one was never written.
    pub(super) fn settle_snippet_note_if_answered(&mut self, cx: &mut Context<Self>) {
        let Some(note) = self.snippet_note else {
            return;
        };
        let waiting = self
            .pane_for(note.session)
            .is_some_and(|pane| pane.read(cx).awaits_paste_confirmation());
        if !waiting {
            self.settle_snippet_note(cx);
        }
    }

    /// The keys the manager reads before its fields do.
    ///
    /// The list is walkable: Up and Down move the highlighted command and Enter inserts it
    /// (the safe of its two actions), so a command can be used without a pointer. Escape is
    /// the window's own "close what floats" action, which takes one layer at a time — the
    /// editor, then the deletion question, then the dialog — so a single press never throws
    /// a draft away.
    fn snippet_panel_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        // While the editor is open the keys belong to its fields.
        if self.snippet_panel.is_editing() {
            return;
        }
        let filtered = self
            .snippet_panel
            .filtered_indices(self.snippets.snippets());
        let len = filtered.len();
        let secondary = event.keystroke.modifiers.number_of_modifiers() == 1
            && event.keystroke.modifiers.secondary();
        match key {
            "down" => {
                cx.stop_propagation();
                self.snippet_panel.step(1, len);
                cx.notify();
            }
            "up" => {
                cx.stop_propagation();
                self.snippet_panel.step(-1, len);
                cx.notify();
            }
            "enter" if self.snippet_panel.pending_delete().is_none() => {
                cx.stop_propagation();
                if let Some(&real_index) = filtered.get(self.snippet_panel.selected())
                    && let Some(snippet) = self.snippets.snippets().get(real_index)
                {
                    let id = snippet.id();
                    self.use_snippet(id, secondary, cx);
                }
            }
            _ => {}
        }
    }

    /// The manager: the list of saved commands, the editor and the deletion question.
    fn snippet_tool(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
        close: impl Fn(&mut Self, &mut Context<Self>) + Clone + 'static,
    ) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let count = self.snippets.snippets().len();
        let subtitle = if count == 0 {
            ux.snippet_manage_hint.to_owned()
        } else {
            format!(
                "{} · {}",
                copy::fill(ux.snippet_count, &[("count", &count.to_string())]),
                ux.snippet_manage_hint
            )
        };
        let close_header = close.clone();
        let (header, hint) = self.modal_header(
            Some(Icon::Terminal),
            ux.snippet_manage_title.to_owned(),
            Some(subtitle),
            "snippets.dialog.close",
            cx,
            move |this, cx| close_header(this, cx),
        );

        let mut body = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::SNUG))
            .px(metrics.scale(spacing::LOOSE))
            .pb(metrics.scale(spacing::SNUG))
            .track_focus(&self.snippet_focus)
            .capture_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                this.snippet_panel_key(event, window, cx);
            }));

        // A file that could not be read is named here, and no save is offered over it.
        if let Some(problem) = self.snippets.read_problem() {
            let path = self.snippets.store().path().display().to_string();
            let reason = copy::fill(ux.snippet_unreadable_body, &[("path", &path)]);
            body = body
                .child(self.notice_line(ux.snippet_unreadable_title.to_owned(), Tone::Danger))
                .child(
                    components::banner(Tone::Danger, metrics)
                        .debug_selector(|| "snippets.unreadable".to_owned())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(SharedString::from(reason)),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .child(SharedString::from(problem.to_owned())),
                );
        }
        if let Some(problem) = self.snippets.write_problem() {
            body = body.child(self.notice_line(
                copy::fill(ux.snippet_save_failed_detail, &[("reason", problem)]),
                Tone::Warning,
            ));
        }

        if let Some(editor) = self.snippet_panel.editor() {
            body = body.child(self.snippet_editor_form(editor, cx));
        } else {
            // An empty library says what to do next; a library that could not be read does
            // not, because its file is the thing to deal with and the banner above says so.
            if count == 0 && self.snippets.can_write() {
                body = body.child(
                    components::state_card_with_icon(
                        Tone::Neutral,
                        Some(Icon::Terminal),
                        ux.snippet_empty_title,
                        ux.snippet_empty_body,
                        None,
                        metrics,
                    )
                    .debug_selector(|| "snippets.empty".to_owned()),
                );
            }
            let filtered = self
                .snippet_panel
                .filtered_indices(self.snippets.snippets());
            if count > 0
                && let Some(search) = self.snippet_panel.search_field().cloned()
            {
                body = body.child(
                    div()
                        .flex()
                        .w_full()
                        .min_w_0()
                        .debug_selector(|| "snippets.search".to_owned())
                        .child(search),
                );
            }
            let mut list = div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::HAIRLINE))
                .debug_selector(|| "snippets.list".to_owned());
            if count > 0 && filtered.is_empty() {
                list = list.child(
                    div()
                        .py(metrics.scale(spacing::SNUG))
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "snippets.search.empty".to_owned())
                        .child(ux.snippet_search_empty),
                );
            } else {
                for (pos, &index) in filtered.iter().enumerate() {
                    list = list.child(self.snippet_row(pos, index, filtered.len(), cx));
                }
            }
            body = body.child(list);
            if count > 0 {
                let platform = crate::app::nav::Platform::current();
                let run_key = crate::app::keymap::keystroke_label(
                    &format!("{}-enter", platform.keystroke_modifier()),
                    platform,
                );
                let keys_hint = copy::fill(ux.snippet_keys_hint, &[("run_key", &run_key)]);
                body = body.child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_MUTED))
                        .debug_selector(|| "snippets.keys.hint".to_owned())
                        .child(SharedString::from(keys_hint)),
                );
            }
            if let Some(question) = self.snippet_delete_question(cx) {
                body = body.child(question);
            }
        }

        // Where the commands live, and what must not be kept in one, in the words the
        // editor does not hide behind a link.
        body = body.child(
            div()
                .debug_selector(|| "snippets.storage".to_owned())
                .text_size(metrics.caption_size())
                .text_color(theme::swatch(palette::TEXT_MUTED))
                .child(ux.snippet_storage_note),
        );

        let writable = self.snippets.can_write();
        let new_button = components::button_with_state(
            ux.snippet_new,
            Variant::Primary,
            if writable {
                components::ControlState::Resting
            } else {
                components::ControlState::Disabled
            },
            metrics,
        )
        .h(metrics.compact_control_height())
        .id("snippets.new")
        .debug_selector(|| "snippets.new".to_owned())
        .when(writable, |button| {
            button.on_click(cx.listener(|this, _event, _window, cx| {
                this.snippet_panel.keep();
                this.open_snippet_editor(None, cx);
            }))
        });
        let close_footer = close.clone();
        let footer = components::modal_footer(metrics)
            .justify_between()
            .child(new_button)
            .child(self.text_button(
                SharedString::from("snippets.dialog.footer.close"),
                ux.tool_close,
                Variant::Ghost,
                cx,
                move |this, cx| close_footer(this, cx),
            ));

        self.modal(
            window,
            "snippets.dialog",
            640.0,
            ModalContent {
                header,
                subtitle: hint,
                body: body.into_any_element(),
                footer: Some(footer),
            },
        )
    }

    /// One saved command: what it is, what it runs and what can be done with it.
    fn snippet_row(
        &self,
        pos: usize,
        index: usize,
        visible_len: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let metrics = self.metrics();
        let ux = copy::ux();
        let Some(snippet) = self.snippets.snippets().get(index) else {
            return div().id(SharedString::from(format!("snippet.row.{index}")));
        };
        let id = snippet.id();
        let lines = snippet.line_count();
        let selected = self.snippet_panel.selected() == pos;
        let hover = theme::swatch(palette::SURFACE_HOVER);
        // The name and the body are one line each, ellipsized when the row is narrow: a
        // column that is allowed to wrap instead is squeezed to a few pixels by the four
        // actions beside it, and at 200% text in the minimum window that turned each
        // command's preview into a ribbon of single characters thousands of pixels tall
        // (R4). The share the row keeps for them is a floor, so the actions wrap under
        // them as a group rather than leaving no room for what the row is about.
        let mut detail = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(metrics.scale(SNIPPET_DETAIL_MIN_WIDTH))
            .gap(px(components::HAIRLINE_WIDTH))
            .child(
                div()
                    .truncate()
                    .text_size(metrics.label_size())
                    .text_color(theme::swatch(palette::TEXT_PRIMARY))
                    .child(SharedString::from(snippet.name().to_owned())),
            )
            .child(
                div()
                    .debug_selector({
                        let selector = format!("snippet.preview.{index}");
                        move || selector.clone()
                    })
                    .truncate()
                    .font_family(crate::app::terminal::monospace_family())
                    .text_size(metrics.caption_size())
                    .text_color(theme::swatch(palette::TEXT_SECONDARY))
                    .child(SharedString::from(body_preview(snippet.body(), 96))),
            );
        if lines > 1 {
            detail = detail.child(
                components::chip(
                    copy::fill(ux.snippet_line_count, &[("count", &lines.to_string())]),
                    Tone::Neutral,
                    metrics,
                )
                .debug_selector({
                    let selector = format!("snippet.lines.{index}");
                    move || selector.clone()
                }),
            );
        }
        div()
            .id(SharedString::from(format!("snippet.row.{index}")))
            .debug_selector({
                let selector = format!("snippet.row.{index}");
                move || selector.clone()
            })
            .flex()
            .items_center()
            .flex_wrap()
            .gap(metrics.scale(spacing::TIGHT))
            .px(metrics.scale(spacing::TIGHT))
            .py(metrics.scale(spacing::HAIRLINE + 2.0))
            .rounded(px(radius::SMALL))
            .when(selected, |row| {
                row.bg(theme::swatch(palette::SURFACE_SELECTED))
            })
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _event, _window, cx| {
                this.snippet_panel.select(pos, visible_len);
                cx.notify();
            }))
            .child(detail)
            // What can be done with the command stays together: at 200% text the four
            // labels are wider than the row, so they take a line of their own below the
            // name and body instead of splitting the name into a column of characters.
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(metrics.scale(spacing::TIGHT))
                    // Filling in is the ordinary action and carries the row's weight;
                    // running submits the command, so it stays the quieter one and says
                    // what it will do before it is pressed (R17).
                    .child(self.snippet_row_button(
                        format!("snippet.insert.{index}"),
                        ux.snippet_insert,
                        ux.snippet_fill_hint,
                        id,
                        false,
                        cx,
                    ))
                    .child(self.snippet_row_button(
                        format!("snippet.run.{index}"),
                        ux.snippet_run,
                        ux.snippet_run_hint,
                        id,
                        true,
                        cx,
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("snippet.edit.{index}")),
                        ux.snippet_edit,
                        Variant::Ghost,
                        cx,
                        move |this, cx| this.open_snippet_editor(Some(id), cx),
                    ))
                    .child(self.text_button(
                        SharedString::from(format!("snippet.delete.{index}")),
                        ux.snippet_delete,
                        Variant::Ghost,
                        cx,
                        move |this, cx| {
                            this.snippet_panel.ask_delete(id);
                            this.snippet_panel.select(pos, visible_len);
                            cx.notify();
                        },
                    )),
            )
    }

    /// The fill-in or run button of one row.
    ///
    /// Filling in writes the command and runs nothing, so it is the row's ordinary
    /// action; running also submits it, so it is drawn quieter and explains itself in a
    /// tooltip. The two are never the same control in two moods: a person choosing one
    /// can tell from the words what will happen to the terminal (R17).
    fn snippet_row_button(
        &self,
        selector: String,
        label: &'static str,
        hint: &'static str,
        id: SnippetId,
        execute: bool,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        self.text_button(
            SharedString::from(selector),
            label,
            if execute {
                Variant::Ghost
            } else {
                Variant::Secondary
            },
            cx,
            move |this, cx| this.use_snippet(id, execute, cx),
        )
        .tooltip(components::tooltip(hint))
    }

    /// The editor of one command: its name, its body and what to do with a refused draft.
    fn snippet_editor_form(&self, editor: &SnippetEditor, cx: &mut Context<Self>) -> Div {
        let metrics = self.metrics();
        let ux = copy::ux();
        let form = div()
            .flex()
            .flex_col()
            .gap(metrics.scale(spacing::TIGHT))
            .debug_selector(|| "snippet.editor".to_owned())
            .child(
                components::section_label(
                    if editor.is_new() {
                        ux.snippet_new
                    } else {
                        ux.snippet_edit_title
                    },
                    metrics,
                )
                .debug_selector(|| "snippet.editor.title".to_owned()),
            )
            .child(components::form_field(
                ux.snippet_field_name,
                div()
                    .h(metrics.control_height())
                    .debug_selector(|| "snippet.editor.name".to_owned())
                    .child(editor.name.clone()),
                1.0,
                components::FORM_FIELD_MIN_WIDTH,
                metrics,
            ))
            .child(components::form_field(
                ux.snippet_field_body,
                div()
                    .h(metrics.control_height())
                    .debug_selector(|| "snippet.editor.body".to_owned())
                    .child(editor.body.clone()),
                1.0,
                components::FORM_FIELD_MIN_WIDTH,
                metrics,
            ))
            .child(components::form_hint(ux.snippet_editor_keys, metrics))
            // A draft that is already too long says so while it is being written: an empty
            // name or body is not reported before anything is typed, because a new command
            // starts empty on purpose.
            .children(
                crate::app::snippets::draft_problem(
                    editor.name.read(cx).text(),
                    editor.body.read(cx).text(),
                )
                .filter(|problem| {
                    !matches!(
                        problem,
                        shellune_core::SnippetProblem::EmptyName
                            | shellune_core::SnippetProblem::EmptyBody
                    )
                })
                .map(|problem| {
                    self.notice_line(
                        copy::snippet_problem_text(&problem, self.workspace.locale()),
                        Tone::Warning,
                    )
                    .debug_selector(|| "snippet.editor.limit".to_owned())
                }),
            )
            .children(
                editor
                    .problem
                    .as_ref()
                    .map(|problem| self.notice_line(problem.clone(), Tone::Danger)),
            );
        // A library whose file could not be read offers no save: the change would have
        // nowhere honest to go, and the banner above says which file to repair.
        let writable = self.snippets.can_write();
        let save = components::button_with_state(
            ux.snippet_save,
            Variant::Primary,
            if writable {
                components::ControlState::Resting
            } else {
                components::ControlState::Disabled
            },
            metrics,
        )
        .h(metrics.compact_control_height())
        .id("snippet.editor.save")
        .debug_selector(|| "snippet.editor.save".to_owned())
        .when(writable, |button| {
            button.on_click(cx.listener(|this, _event, _window, cx| {
                this.save_snippet_editor(cx);
            }))
        });
        let cancel = self.text_button(
            SharedString::from("snippet.editor.cancel"),
            ux.fwd_cancel,
            Variant::Ghost,
            cx,
            |this, cx| this.close_snippet_editor(cx),
        );
        form.child(
            div()
                .flex()
                .items_center()
                .w_full()
                .justify_end()
                .gap(metrics.scale(spacing::TIGHT))
                .child(cancel)
                .child(save),
        )
    }

    /// The question that stands between a command and its deletion.
    fn snippet_delete_question(&self, cx: &mut Context<Self>) -> Option<Div> {
        let id = self.snippet_panel.pending_delete()?;
        let snippet = self.snippets.get(id)?;
        let metrics = self.metrics();
        let ux = copy::ux();
        let name = snippet.name().to_owned();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(metrics.scale(spacing::TIGHT))
                .p(metrics.scale(spacing::SNUG))
                .rounded(px(radius::SMALL))
                .border_1()
                .border_color(theme::swatch(palette::DANGER))
                .bg(theme::swatch(palette::DANGER_TINT))
                .debug_selector(|| "snippet.delete.question".to_owned())
                .child(
                    div()
                        .text_size(metrics.label_size())
                        .text_color(theme::swatch(palette::TEXT_PRIMARY))
                        .child(ux.snippet_delete_title),
                )
                .child(
                    div()
                        .text_size(metrics.caption_size())
                        .text_color(theme::swatch(palette::TEXT_SECONDARY))
                        .child(SharedString::from(copy::fill(
                            ux.snippet_delete_body,
                            &[("name", &name)],
                        ))),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .justify_end()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(self.text_button(
                            SharedString::from("snippet.delete.no"),
                            ux.snippet_delete_no,
                            Variant::Ghost,
                            cx,
                            |this, cx| {
                                this.snippet_panel.keep();
                                cx.notify();
                            },
                        ))
                        .child(self.text_button(
                            SharedString::from("snippet.delete.yes"),
                            ux.snippet_delete_yes,
                            Variant::Danger,
                            cx,
                            |this, cx| this.confirm_snippet_delete(cx),
                        )),
                ),
        )
    }
}

/// A soft pulse for the step that is running: it says "working" without a bar that
/// claims a percentage nobody measured. Still when motion is reduced.
fn progress_pulse(metrics: theme::Metrics) -> AnyElement {
    let dot = div()
        .size(metrics.scale(PULSE_DOT_SIZE))
        .rounded_full()
        .bg(theme::swatch(palette::ACCENT));
    if theme::motion_allowed() {
        gpui::AnimationExt::with_animation(
            dot,
            "remote.progress.pulse",
            gpui::Animation::new(theme::Motion::PULSE)
                .repeat()
                .with_easing(gpui::pulsating_between(0.35, 1.0)),
            |dot, delta| dot.opacity(delta),
        )
        .into_any_element()
    } else {
        dot.into_any_element()
    }
}
