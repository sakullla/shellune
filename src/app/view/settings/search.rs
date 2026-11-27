use super::*;
use crate::app::text_field::TextField;
use gpui::{Entity, FocusHandle, KeyDownEvent, ScrollHandle};

pub(super) struct SettingsSearch {
    field: Entity<TextField>,
    query: String,
    selected: usize,
    scroll: ScrollHandle,
    return_focus: FocusHandle,
    return_offset: gpui::Point<gpui::Pixels>,
    cancel_focus: FocusHandle,
    _leave_page: gpui::Subscription,
}

fn title(locale: Locale) -> &'static str {
    match locale {
        Locale::Chinese => "搜索设置",
        Locale::English => "Search settings",
    }
}

fn matches(locale: Locale, query: &str) -> Vec<Item> {
    let query = query.trim().to_lowercase();
    Category::ALL
        .into_iter()
        .flat_map(Category::items)
        .copied()
        .filter(|item| {
            let aliases = match (item, locale) {
                (Item::TextSize | Item::FontSize, Locale::Chinese) => "字号 字体 字体大小",
                (Item::TextSize | Item::FontSize, Locale::English) => "font size text size",
                _ => "",
            };
            [
                item.label(locale),
                item.hint(locale),
                item.category().label(locale),
                aliases,
            ]
            .iter()
            .any(|value| value.to_lowercase().contains(&query))
        })
        .collect()
}

impl ShelluneView {
    pub(in crate::app::view) fn settings_target_focus(&self, id: &str) -> Option<FocusHandle> {
        self.settings_nav
            .control_focus
            .as_ref()
            .filter(|(target, _)| *target == id)
            .map(|(_, focus)| focus.clone())
    }

    pub(super) fn settings_search_button(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = title(self.workspace.locale());
        components::icon_label_button(
            Icon::Search,
            label,
            components::Variant::Secondary,
            self.metrics(),
        )
        .id("settings.search.open")
        .debug_selector(|| "settings.search.open".to_owned())
        .tooltip(components::tooltip(format!(
            "{label} ({})",
            keymap::keystroke_label(
                if self.keymap.platform() == crate::app::Platform::MacOs {
                    "cmd-f"
                } else {
                    "ctrl-f"
                },
                self.keymap.platform()
            )
        )))
        // A pointer press should remember the previously focused control.
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.prevent_default()
        })
        .on_click(cx.listener(|view, _, window, cx| view.open_settings_search(window, cx)))
    }

    fn open_settings_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.settings_nav.search {
            window.focus(search.field.read(cx).focus_handle());
            return;
        }
        let label = title(self.workspace.locale());
        let field = cx.new(|cx| {
            let mut field = TextField::new(label, cx);
            field.set_leading_icon(Icon::Search, cx);
            field.set_clearable(true, cx);
            field
        });
        cx.observe(&field, |view, field, cx| {
            if let Some(search) = &mut view.settings_nav.search {
                let query = field.read(cx).text();
                if query != search.query {
                    search.query = query.to_owned();
                    search.selected = 0;
                    search.scroll.scroll_to_top_of_item(0);
                    cx.notify();
                }
            }
        })
        .detach();
        let previous = window
            .focused(cx)
            .unwrap_or_else(|| self.destination_focus(Destination::Settings).clone());
        let leave_page = cx.on_focus_out(
            self.destination_focus(Destination::Settings),
            window,
            |view, _, _, cx| {
                if view.workspace.destination() != Destination::Settings
                    && view.settings_nav.search.take().is_some()
                {
                    cx.notify();
                }
            },
        );
        window.focus(field.read(cx).focus_handle());
        self.settings_nav.search = Some(SettingsSearch {
            field,
            query: String::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            return_focus: previous,
            return_offset: self.settings_scroll.offset(),
            cancel_focus: cx.focus_handle(),
            _leave_page: leave_page,
        });
        cx.notify();
    }

    fn dismiss_settings_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = self.settings_nav.search.take() {
            self.settings_pending_reveal.set(false);
            self.settings_scroll.set_offset(search.return_offset);
            window.focus(&search.return_focus);
            cx.notify();
        }
    }

    fn choose_settings_result(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_nav.search = None;
        self.settings_nav.control_focus = None;
        let category = item.category();
        let index = category
            .items()
            .iter()
            .position(|candidate| *candidate == item)
            .unwrap();
        self.settings_select_item(category, index, cx);
        // Text fields already own a focus handle. Buttons, steppers and segmented
        // choices attach this handle to their actual control when the page redraws.
        let control = |view: &mut Self, id| {
            let handle = cx.focus_handle();
            view.settings_nav.control_focus = Some((id, handle.clone()));
            handle
        };
        let handle = match item {
            Item::Shortcuts => self.keys_search.read(cx).focus_handle().clone(),
            Item::HostTrust => self.settings_trust_field_focus(cx),
            Item::KeysAndPassword if !self.vault_locked() => self.security_search_focus(cx),
            Item::Sync => self.settings_sync_url_focus(cx),
            Item::Theme => control(self, "settings.theme"),
            Item::Language => control(self, "settings.language"),
            Item::TextSize => control(self, "settings.zoom"),
            Item::Motion => control(self, "settings.motion"),
            Item::OsIcons => control(self, "settings.os_icons"),
            Item::Scheme => control(self, "settings.scheme"),
            Item::FontSize => control(self, "settings.font.more"),
            Item::DefaultShell => control(self, "settings.shell"),
            Item::Highlight => control(self, "settings.highlight"),
            Item::Policy => control(self, "settings.keepalive"),
            Item::KeysAndPassword => control(self, "security.locked.unlock"),
            Item::Lock => control(self, "security.lock.biometric"),
            Item::BellNotice => control(self, "settings.notice.bell"),
            Item::FinishedNotice => control(self, "settings.notice.finished"),
            Item::DesktopNotify => control(self, "settings.notify.program"),
            Item::HostsFile => control(self, "settings.paths"),
            Item::Import => control(self, "settings.import"),
            Item::ExportBackup => control(self, "settings.backup.export"),
            Item::ImportBackup => control(self, "settings.backup.import"),
            Item::Version => control(self, "status.version"),
            Item::Update => control(self, "settings.update.auto"),
        };
        window.focus(&handle);
        cx.notify();
    }

    pub(super) fn settings_search_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.key_capture.is_some() {
            return;
        }
        let stroke = &event.keystroke;
        let modifiers = &stroke.modifiers;
        if event.is_held && stroke.key == "enter" && self.settings_nav.consume_search_enter_release
        {
            window.prevent_default();
            cx.stop_propagation();
            return;
        }
        if !event.is_held {
            self.settings_nav.consume_search_enter_release = false;
        }
        if modifiers.secondary() && modifiers.number_of_modifiers() == 1 && stroke.key == "f" {
            self.open_settings_search(window, cx);
            cx.stop_propagation();
            return;
        }
        let Some(search) = &mut self.settings_nav.search else {
            return;
        };
        if stroke.key == "tab" && !modifiers.control && !modifiers.alt && !modifiers.platform {
            let target = if search.cancel_focus.is_focused(window) {
                search.field.read(cx).focus_handle()
            } else {
                &search.cancel_focus
            };
            window.focus(target);
            cx.stop_propagation();
            return;
        }
        if modifiers.modified() {
            return;
        }
        let results = matches(self.workspace.locale(), search.field.read(cx).text());
        let selected = search.selected.min(results.len().saturating_sub(1));
        match stroke.key.as_str() {
            "escape" => self.dismiss_settings_search(window, cx),
            "up" | "down" => {
                search.selected = if stroke.key == "up" {
                    selected.saturating_sub(1)
                } else {
                    (selected + 1).min(results.len().saturating_sub(1))
                };
                search.scroll.scroll_to_item(search.selected);
                cx.notify();
            }
            "enter" => {
                if search.cancel_focus.is_focused(window) {
                    self.dismiss_settings_search(window, cx);
                    self.settings_nav.consume_search_enter_release = true;
                } else if let Some(item) = results.get(selected) {
                    self.choose_settings_result(*item, window, cx);
                    self.settings_nav.consume_search_enter_release = true;
                }
            }
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    pub(super) fn settings_search_results(&self, cx: &mut Context<Self>) -> Option<Div> {
        let search = self.settings_nav.search.as_ref()?;
        let locale = self.workspace.locale();
        let metrics = self.metrics();
        search
            .field
            .update(cx, |field, cx| field.set_placeholder(title(locale), cx));
        let results = matches(locale, search.field.read(cx).text());
        let marked = search.selected.min(results.len().saturating_sub(1));
        let mut list = div()
            .id("settings.search.results")
            .track_scroll(&search.scroll)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scroll()
            .gap(metrics.scale(spacing::HALF));
        if results.is_empty() {
            list = list.child(
                components::form_hint(copy::ux_for(locale).picker_no_match, metrics)
                    .debug_selector(|| "settings.search.empty".to_owned()),
            );
        }
        for (index, item) in results.into_iter().enumerate() {
            let selected = index == marked;
            list = list.child(
                components::menu_item(item.label(locale), selected, metrics)
                    .id(("settings.search.result", index))
                    .debug_selector(move || format!("settings.search.result.{}", item.id()))
                    .child(components::menu_shortcut(
                        item.category().label(locale),
                        metrics,
                    ))
                    .when(selected, |row| {
                        row.border_l_2()
                            .border_color(theme::swatch(palette::FOCUS_RING))
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.choose_settings_result(item, window, cx)
                    })),
            );
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .on_action(cx.listener(
                    |view, _: &crate::app::actions::DismissNotice, window, cx| {
                        view.dismiss_settings_search(window, cx);
                    },
                ))
                .flex()
                .flex_col()
                .min_w_0()
                .min_h_0()
                .bg(theme::swatch(palette::CANVAS))
                .p(metrics.scale(spacing::LOOSE))
                .gap(metrics.scale(spacing::BASE))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(metrics.scale(spacing::TIGHT))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .debug_selector(|| "settings.search.input".to_owned())
                                .child(search.field.clone()),
                        )
                        .child(
                            components::compact_button(
                                self.strings().host_cancel,
                                components::Variant::Secondary,
                                metrics,
                            )
                            .id("settings.search.cancel")
                            .track_focus(&search.cancel_focus)
                            .focus(super::super::files_focus_ring)
                            .debug_selector(|| "settings.search.cancel".to_owned())
                            .on_click(cx.listener(
                                |view, _, window, cx| view.dismiss_settings_search(window, cx),
                            )),
                        ),
                )
                .child(list),
        )
    }

    #[cfg(feature = "ui-capture")]
    pub(crate) fn capture_settings_item_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_settings_search(window, cx);
        let query = match self.workspace.locale() {
            Locale::English => "font",
            Locale::Chinese => "字体",
        };
        if let Some(search) = &self.settings_nav.search {
            search
                .field
                .update(cx, |field, cx| field.set_text(query, cx));
        }
    }

    #[cfg(feature = "ui-capture")]
    pub(crate) fn capture_settings_font_results_ready(&self, cx: &App) -> bool {
        self.settings_nav.search.as_ref().is_some_and(|search| {
            let text = search.field.read(cx).text();
            let rows = matches(self.workspace.locale(), text);
            !text.is_empty() && rows.contains(&Item::TextSize) && rows.contains(&Item::FontSize)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Modifiers, TestAppContext, VisualTestContext, size};

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
    }

    #[test]
    fn workspace_ux_settings_matches_names_hints_categories_and_local_font_terms() {
        for (locale, query) in [(Locale::English, " FONT "), (Locale::Chinese, "字体")] {
            assert_eq!(matches(locale, query), [Item::TextSize, Item::FontSize]);
            assert!(matches(locale, Item::Motion.hint(locale)).contains(&Item::Motion));
            assert!(matches(locale, Category::Connection.label(locale)).contains(&Item::Policy));
        }
    }

    #[gpui::test]
    async fn workspace_ux_settings_search_navigates_to_the_real_font_control(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|_, cx| ShelluneView::new(cx));
        cx.simulate_resize(size(px(720.), px(480.)));
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.workspace.set_locale(Locale::Chinese);
                v.workspace.set_zoom(Zoom::Comfortable);
                v.select(Destination::Settings, window, cx);
            })
        });
        cx.run_until_parked();
        click(cx, "settings.search.open");
        let font = cx.update(|_, app| view.read(app).workspace.local_font_size());
        cx.simulate_input("字体");
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("settings.search.result.text-size")
                .is_some()
        );
        assert!(
            cx.debug_bounds("settings.search.result.font-size")
                .is_some()
        );
        cx.simulate_keystrokes("down enter");
        cx.run_until_parked();
        cx.simulate_event(gpui::KeyUpEvent {
            keystroke: gpui::Keystroke::parse("enter").unwrap(),
        });
        let control = cx.debug_bounds("settings.font.more").expect("font stepper");
        let clip = cx
            .debug_bounds("settings.scroll")
            .expect("settings viewport");
        assert!(
            control.origin.y >= clip.origin.y && control.bottom() <= clip.bottom(),
            "the focused stepper must be visible: {control:?} in {clip:?}"
        );
        cx.update(|window, app| {
            let v = view.read(app);
            assert_eq!(v.settings_nav.current(), Item::FontSize);
            assert!(
                v.settings_target_focus("settings.font.more")
                    .unwrap()
                    .is_focused(window)
            );
            assert_eq!(v.workspace.local_font_size(), font);
        });
        cx.simulate_keystrokes("right");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, app| view.read(app).workspace.local_font_size()),
            font + 1.
        );
    }

    #[gpui::test]
    async fn workspace_ux_settings_search_cancel_restores_scroll_and_input_and_pointer_can_choose(
        cx: &mut TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(|_, cx| ShelluneView::new(cx));
        cx.simulate_resize(size(px(1120.), px(720.)));
        cx.update(|_, app| {
            app.bind_keys(crate::app::key_bindings(crate::app::Platform::current()))
        });
        cx.update(|window, app| {
            view.update(app, |v, cx| {
                v.workspace.set_locale(Locale::English);
                v.select(Destination::Settings, window, cx);
                v.settings_select_item(Category::Keys, 0, cx);
                window.focus(v.keys_search.read(cx).focus_handle());
            })
        });
        cx.run_until_parked();
        let origin = cx.update(|_, app| view.read(app).settings_scroll.offset());
        let key = if crate::app::nav::Platform::current() == crate::app::nav::Platform::MacOs {
            "cmd-f"
        } else {
            "ctrl-f"
        };
        cx.simulate_keystrokes(key);
        cx.simulate_input("zzznomatches");
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings.search.empty").is_some());
        cx.simulate_keystrokes("enter escape");
        cx.run_until_parked();
        cx.update(|window, app| {
            let v = view.read(app);
            assert!(v.settings_nav.search.is_none());
            assert_eq!(v.settings_nav.current(), Item::Shortcuts);
            assert!(v.keys_search.read(app).focus_handle().is_focused(window));
            assert_eq!(v.settings_scroll.offset(), origin);
        });
        cx.simulate_keystrokes(key);
        cx.simulate_input("font");
        cx.run_until_parked();
        click(cx, "settings.search.result.text-size");
        let zoom = cx.update(|window, app| {
            let v = view.read(app);
            assert!(
                v.settings_target_focus("settings.zoom")
                    .unwrap()
                    .is_focused(window)
            );
            v.workspace.zoom()
        });
        cx.simulate_keystrokes(if zoom == Zoom::Comfortable {
            "left"
        } else {
            "right"
        });
        cx.run_until_parked();
        assert_ne!(cx.update(|_, app| view.read(app).workspace.zoom()), zoom);
    }
}
