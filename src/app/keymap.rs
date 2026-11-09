//! The action catalogue and the key bindings in force (R3—R5).
//!
//! Every shortcut the window offers is one entry of [`ACTIONS`]: a stable
//! identifier, the input context it applies in, its names in both languages and the
//! default keystrokes for each platform. Nothing else in the application writes a
//! shortcut string down, so the settings page, the command palette, the tooltips and
//! the GPUI key map can never disagree about which key does what.
//!
//! The bindings actually in force are a [`Keymap`]: the catalogue's defaults with the
//! user's changes laid over them. A change is a keystroke replacement or an explicit
//! unbinding, which is how a combination is handed back to a terminal program. Two
//! actions may never share a keystroke in contexts that can be true at the same time;
//! a saved set that does is replaced by the defaults and reported instead of being
//! half-applied.
//!
//! Escape, Ctrl+C and Ctrl+L never enter the catalogue: they belong to the program
//! running in a terminal, and an action list that could take them away would let a
//! setting break the shell.

use gpui::{KeyBinding, Keystroke};

use crate::app::actions::{
    CloseActiveTab, CycleTextSize, DismissNotice, FocusPaneDown, FocusPaneLeft, FocusPaneRight,
    FocusPaneUp, GoToHosts, GoToLocalShells, GoToSettings, MovePaneDown, MovePaneLeft,
    MovePaneRight, MovePaneUp, MoveTabLeft, MoveTabRight, NewLocalShell, NextDestination, NextTab,
    OpenPalette, PreviousDestination, PreviousTab, ResizePaneDown, ResizePaneLeft, ResizePaneRight,
    ResizePaneUp, SelectTab1, SelectTab2, SelectTab3, SelectTab4, SelectTab5, SelectTab6,
    SelectTab7, SelectTab8, SelectTab9, SplitPaneDown, SplitPaneRight, TerminalCopy, TerminalPaste,
    TerminalSearch, ToggleLanguage, ToggleSidebar, ZoomPane,
};
use crate::app::copy::Locale;
use crate::app::nav::{Destination, Platform};
use shellune_storage::{KeymapEntry, KeymapPreferences};

/// Key context of the whole shell, terminals included.
pub const KEY_CONTEXT: &str = "Shellune";

/// Key context of a terminal pane's grid.
///
/// A shortcut bound to [`KEY_CONTEXT`] is resolved before the pane ever sees the key,
/// so the combinations a program needs must not be bound there. Two matter most:
/// Escape, because it is how Vim and less leave insert mode, and Ctrl+L, which the
/// shell reads as "clear the screen".
pub const TERMINAL_KEY_CONTEXT: &str = "Terminal";

/// The context the shell's own shortcuts apply in: everywhere but a terminal.
///
/// Written as a predicate rather than a plain identifier because a pane is inside the
/// shell context too, so excluding it has to be said out loud.
pub const APP_KEY_CONTEXT: &str = "Shellune && !Terminal";

/// The input context an action's shortcut applies in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Context {
    /// Anywhere in the shell, termininals included: a combination no shell program
    /// reads on its own.
    Shell,
    /// In the shell, but not while a terminal has the keyboard.
    OutsideTerminal,
    /// Inside a terminal, for the actions that act on the program's screen.
    Terminal,
}

impl Context {
    /// Every context, in the order the settings list them.
    pub const ALL: [Self; 3] = [Self::Terminal, Self::Shell, Self::OutsideTerminal];

    /// The identifier written next to a saved binding.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::OutsideTerminal => "outside-terminal",
            Self::Terminal => "terminal",
        }
    }

    /// The context read back from a saved binding, if this build still knows it.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|context| context.id() == id)
    }

    /// The predicate GPUI matches against the focus path.
    #[must_use]
    pub fn predicate(self) -> &'static str {
        match self {
            Self::Shell => KEY_CONTEXT,
            Self::OutsideTerminal => APP_KEY_CONTEXT,
            Self::Terminal => TERMINAL_KEY_CONTEXT,
        }
    }

    /// Name of the context in the current language.
    #[must_use]
    pub fn label(self, locale: Locale) -> &'static str {
        match (self, locale) {
            (Self::Terminal, Locale::English) => "In a terminal",
            (Self::Terminal, Locale::Chinese) => "终端内",
            (Self::Shell, Locale::English) => "Everywhere",
            (Self::Shell, Locale::Chinese) => "所有位置",
            (Self::OutsideTerminal, Locale::English) => "Outside a terminal",
            (Self::OutsideTerminal, Locale::Chinese) => "终端外",
        }
    }

    /// Whether a key bound in this context can also be pressed while the other one
    /// holds, which is what makes two actions collide.
    ///
    /// A terminal pane sits inside the shell, so a shell-wide binding is resolved
    /// before the pane sees the key. The two "terminal" and "outside a terminal" parts
    /// are exclusive by construction.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        match (self, other) {
            (left, right) if left == right => true,
            (Self::Terminal, Self::OutsideTerminal) | (Self::OutsideTerminal, Self::Terminal) => {
                false
            }
            _ => true,
        }
    }
}

/// A default binding written as something the platform decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultKey {
    /// The same keystroke on every platform.
    Literal(&'static str),
    /// The platform's command modifier in front of a key: Ctrl on Windows and Linux,
    /// Cmd on macOS.
    Modifier(&'static str),
    /// A page's own number shortcut.
    Page(Destination),
    /// The platform's new-shell shortcut.
    NewShell,
    /// The platform's command palette shortcut.
    Palette,
    /// The platform's close-session shortcut.
    CloseSession,
    /// The platform's next and previous tab shortcuts.
    NextTab,
    PreviousTab,
    /// Jumping straight to tab `n`, one-based.
    TabJump(usize),
}

impl DefaultKey {
    /// The keystrokes of this default on one platform.
    #[must_use]
    pub fn keystrokes(self, platform: Platform) -> Vec<String> {
        match self {
            Self::Literal(keystroke) => vec![(*keystroke).to_owned()],
            Self::Modifier(key) => vec![format!("{}-{key}", platform.keystroke_modifier())],
            Self::Page(destination) => vec![destination.keystroke(platform)],
            Self::NewShell => vec![platform.new_shell_keystroke().to_owned()],
            Self::Palette => vec![platform.palette_keystroke().to_owned()],
            Self::CloseSession => vec![platform.close_tab_keystroke().to_owned()],
            Self::NextTab => vec![platform.tab_cycle_keystrokes().0.to_owned()],
            Self::PreviousTab => vec![platform.tab_cycle_keystrokes().1.to_owned()],
            Self::TabJump(number) => vec![format!("{}-{number}", platform.tab_jump_modifier())],
        }
    }
}

/// One action of the catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionSpec {
    /// Stable identifier, written next to a saved binding.
    pub id: &'static str,
    /// Where the shortcut applies.
    pub context: Context,
    /// Name in English.
    pub name_en: &'static str,
    /// Name in Chinese.
    pub name_zh: &'static str,
    /// Default keystrokes; more than one means the action has an alias.
    pub defaults: &'static [DefaultKey],
}

impl ActionSpec {
    /// The action's name in the current language.
    #[must_use]
    pub fn name(&self, locale: Locale) -> &'static str {
        match locale {
            Locale::English => self.name_en,
            Locale::Chinese => self.name_zh,
        }
    }

    /// The default keystrokes on one platform.
    #[must_use]
    pub fn default_keystrokes(&self, platform: Platform) -> Vec<String> {
        self.defaults
            .iter()
            .flat_map(|default| default.keystrokes(platform))
            .collect()
    }
}

/// Every shortcut the shell offers, in the order the settings page lists them.
///
/// The window actions come first, then the terminal ones. Nothing here may take a key
/// a program reads on its own while it runs; the terminal entries use the shifted
/// forms for exactly that reason.
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        id: "nav.local",
        context: Context::Shell,
        name_en: "Go to local shells",
        name_zh: "前往本地 Shell",
        defaults: &[DefaultKey::Page(Destination::LocalShells)],
    },
    ActionSpec {
        id: "nav.hosts",
        context: Context::Shell,
        name_en: "Go to hosts",
        name_zh: "前往主机",
        defaults: &[DefaultKey::Page(Destination::Hosts)],
    },
    ActionSpec {
        id: "nav.settings",
        context: Context::Shell,
        name_en: "Go to settings",
        name_zh: "前往设置",
        defaults: &[
            DefaultKey::Page(Destination::Settings),
            DefaultKey::Modifier(","),
        ],
    },
    ActionSpec {
        id: "nav.next",
        context: Context::Shell,
        name_en: "Next page",
        name_zh: "下一个页面",
        defaults: &[DefaultKey::Modifier("tab")],
    },
    ActionSpec {
        id: "nav.previous",
        context: Context::Shell,
        name_en: "Previous page",
        name_zh: "上一个页面",
        defaults: &[DefaultKey::Modifier("shift-tab")],
    },
    ActionSpec {
        id: "text.size",
        context: Context::Shell,
        name_en: "Interface text size",
        name_zh: "界面文字大小",
        defaults: &[DefaultKey::Modifier("=")],
    },
    ActionSpec {
        id: "shell.new",
        context: Context::Shell,
        name_en: "New local shell",
        name_zh: "新建本地 Shell",
        defaults: &[DefaultKey::NewShell],
    },
    ActionSpec {
        id: "language.toggle",
        context: Context::OutsideTerminal,
        name_en: "Switch language",
        name_zh: "切换界面语言",
        defaults: &[DefaultKey::Modifier("l")],
    },
    ActionSpec {
        id: "notice.dismiss",
        context: Context::OutsideTerminal,
        name_en: "Dismiss notice or menu",
        name_zh: "关闭提示或菜单",
        defaults: &[DefaultKey::Literal("escape")],
    },
    ActionSpec {
        id: "palette.open",
        context: Context::Shell,
        name_en: "Command palette",
        name_zh: "命令面板",
        defaults: &[DefaultKey::Palette],
    },
    ActionSpec {
        id: "tab.close",
        context: Context::Shell,
        name_en: "Close current session",
        name_zh: "关闭当前会话",
        defaults: &[DefaultKey::CloseSession],
    },
    ActionSpec {
        id: "tab.next",
        context: Context::Shell,
        name_en: "Next tab",
        name_zh: "下一个标签",
        defaults: &[DefaultKey::NextTab],
    },
    ActionSpec {
        id: "tab.previous",
        context: Context::Shell,
        name_en: "Previous tab",
        name_zh: "上一个标签",
        defaults: &[DefaultKey::PreviousTab],
    },
    ActionSpec {
        id: "tab.move-left",
        context: Context::Shell,
        name_en: "Move tab left",
        name_zh: "标签左移",
        defaults: &[DefaultKey::Literal("alt-shift-,")],
    },
    ActionSpec {
        id: "tab.move-right",
        context: Context::Shell,
        name_en: "Move tab right",
        name_zh: "标签右移",
        defaults: &[DefaultKey::Literal("alt-shift-.")],
    },
    ActionSpec {
        id: "tab.select.1",
        context: Context::Shell,
        name_en: "Go to tab 1",
        name_zh: "跳到标签 1",
        defaults: &[DefaultKey::TabJump(1)],
    },
    ActionSpec {
        id: "tab.select.2",
        context: Context::Shell,
        name_en: "Go to tab 2",
        name_zh: "跳到标签 2",
        defaults: &[DefaultKey::TabJump(2)],
    },
    ActionSpec {
        id: "tab.select.3",
        context: Context::Shell,
        name_en: "Go to tab 3",
        name_zh: "跳到标签 3",
        defaults: &[DefaultKey::TabJump(3)],
    },
    ActionSpec {
        id: "tab.select.4",
        context: Context::Shell,
        name_en: "Go to tab 4",
        name_zh: "跳到标签 4",
        defaults: &[DefaultKey::TabJump(4)],
    },
    ActionSpec {
        id: "tab.select.5",
        context: Context::Shell,
        name_en: "Go to tab 5",
        name_zh: "跳到标签 5",
        defaults: &[DefaultKey::TabJump(5)],
    },
    ActionSpec {
        id: "tab.select.6",
        context: Context::Shell,
        name_en: "Go to tab 6",
        name_zh: "跳到标签 6",
        defaults: &[DefaultKey::TabJump(6)],
    },
    ActionSpec {
        id: "tab.select.7",
        context: Context::Shell,
        name_en: "Go to tab 7",
        name_zh: "跳到标签 7",
        defaults: &[DefaultKey::TabJump(7)],
    },
    ActionSpec {
        id: "tab.select.8",
        context: Context::Shell,
        name_en: "Go to tab 8",
        name_zh: "跳到标签 8",
        defaults: &[DefaultKey::TabJump(8)],
    },
    ActionSpec {
        id: "tab.select.9",
        context: Context::Shell,
        name_en: "Go to the last tab",
        name_zh: "跳到最后一个标签",
        defaults: &[DefaultKey::TabJump(9)],
    },
    ActionSpec {
        id: "sidebar.toggle",
        context: Context::Shell,
        name_en: "Collapse or expand the sidebar",
        name_zh: "折叠或展开侧栏",
        defaults: &[],
    },
    ActionSpec {
        id: "pane.split-right",
        context: Context::Shell,
        name_en: "Split pane to the right",
        name_zh: "向右拆分窗格",
        defaults: &[DefaultKey::Literal("alt-shift-=")],
    },
    ActionSpec {
        id: "pane.split-down",
        context: Context::Shell,
        name_en: "Split pane downwards",
        name_zh: "向下拆分窗格",
        defaults: &[DefaultKey::Literal("alt-shift--")],
    },
    ActionSpec {
        id: "pane.zoom",
        context: Context::Shell,
        name_en: "Zoom the pane",
        name_zh: "缩放窗格",
        defaults: &[DefaultKey::Literal("alt-shift-z")],
    },
    ActionSpec {
        id: "pane.focus-left",
        context: Context::Shell,
        name_en: "Focus the pane on the left",
        name_zh: "焦点移到左侧窗格",
        defaults: &[DefaultKey::Literal("alt-left")],
    },
    ActionSpec {
        id: "pane.focus-right",
        context: Context::Shell,
        name_en: "Focus the pane on the right",
        name_zh: "焦点移到右侧窗格",
        defaults: &[DefaultKey::Literal("alt-right")],
    },
    ActionSpec {
        id: "pane.focus-up",
        context: Context::Shell,
        name_en: "Focus the pane above",
        name_zh: "焦点移到上方窗格",
        defaults: &[DefaultKey::Literal("alt-up")],
    },
    ActionSpec {
        id: "pane.focus-down",
        context: Context::Shell,
        name_en: "Focus the pane below",
        name_zh: "焦点移到下方窗格",
        defaults: &[DefaultKey::Literal("alt-down")],
    },
    ActionSpec {
        id: "pane.resize-left",
        context: Context::Shell,
        name_en: "Resize the pane to the left",
        name_zh: "向左调整窗格大小",
        defaults: &[DefaultKey::Literal("alt-shift-left")],
    },
    ActionSpec {
        id: "pane.resize-right",
        context: Context::Shell,
        name_en: "Resize the pane to the right",
        name_zh: "向右调整窗格大小",
        defaults: &[DefaultKey::Literal("alt-shift-right")],
    },
    ActionSpec {
        id: "pane.resize-up",
        context: Context::Shell,
        name_en: "Resize the pane upwards",
        name_zh: "向上调整窗格大小",
        defaults: &[DefaultKey::Literal("alt-shift-up")],
    },
    ActionSpec {
        id: "pane.resize-down",
        context: Context::Shell,
        name_en: "Resize the pane downwards",
        name_zh: "向下调整窗格大小",
        defaults: &[DefaultKey::Literal("alt-shift-down")],
    },
    ActionSpec {
        id: "pane.move-left",
        context: Context::Shell,
        name_en: "Move the pane left",
        name_zh: "窗格左移",
        defaults: &[DefaultKey::Literal("ctrl-alt-left")],
    },
    ActionSpec {
        id: "pane.move-right",
        context: Context::Shell,
        name_en: "Move the pane right",
        name_zh: "窗格右移",
        defaults: &[DefaultKey::Literal("ctrl-alt-right")],
    },
    ActionSpec {
        id: "pane.move-up",
        context: Context::Shell,
        name_en: "Move the pane up",
        name_zh: "窗格上移",
        defaults: &[DefaultKey::Literal("ctrl-alt-up")],
    },
    ActionSpec {
        id: "pane.move-down",
        context: Context::Shell,
        name_en: "Move the pane down",
        name_zh: "窗格下移",
        defaults: &[DefaultKey::Literal("ctrl-alt-down")],
    },
    ActionSpec {
        id: "terminal.search",
        context: Context::Terminal,
        name_en: "Search this terminal",
        name_zh: "搜索当前终端",
        defaults: &[DefaultKey::Modifier("shift-f")],
    },
    ActionSpec {
        id: "terminal.copy",
        context: Context::Terminal,
        name_en: "Copy the selection",
        name_zh: "复制选中内容",
        defaults: &[DefaultKey::Modifier("shift-c")],
    },
    ActionSpec {
        id: "terminal.paste",
        context: Context::Terminal,
        name_en: "Paste into the terminal",
        name_zh: "粘贴到终端",
        defaults: &[DefaultKey::Modifier("shift-v")],
    },
];

/// The catalogue entry with this identifier.
#[must_use]
pub fn action(id: &str) -> Option<&'static ActionSpec> {
    ACTIONS.iter().find(|spec| spec.id == id)
}

/// Position of an action in [`ACTIONS`].
fn index_of(id: &str) -> Option<usize> {
    ACTIONS.iter().position(|spec| spec.id == id)
}

/// The catalogue entry that a page's number shortcut belongs to.
#[must_use]
pub fn page_action(destination: Destination) -> Option<&'static ActionSpec> {
    let id = match destination {
        Destination::LocalShells => "nav.local",
        Destination::Hosts => "nav.hosts",
        Destination::Settings => "nav.settings",
        Destination::Sessions | Destination::Transfers | Destination::Forwards => return None,
    };
    action(id)
}

/// The catalogue entry for jumping to a tab by its one-based position.
#[must_use]
pub fn tab_jump_action(number: usize) -> Option<&'static ActionSpec> {
    action(&format!("tab.select.{number}"))
}

/// Why a binding could not be applied. Every variant is something the user can read
/// and, in the settings page, act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// A saved binding names an action this build no longer has.
    UnknownAction(String),
    /// A saved binding was made in a context that action no longer uses.
    ContextChanged(String),
    /// A saved or captured keystroke is not a combination GPUI can bind.
    InvalidKeystroke(String),
    /// Two actions would answer the same key in contexts that can hold at once.
    Conflict {
        /// The contested keystroke, in the GPUI keymap syntax.
        keystroke: String,
        /// Identifier of the first action.
        first: &'static str,
        /// Identifier of the second action.
        second: &'static str,
    },
    /// A captured combination carries no modifier, so it would take a typed key away
    /// from the terminal program.
    NeedsModifier(String),
    /// The preferences file could not be read at all.
    UnreadablePreferences,
}

impl Problem {
    /// The problem in the current language.
    #[must_use]
    pub fn text(&self, locale: Locale, platform: Platform) -> String {
        let name = |id: &str| {
            action(id).map_or_else(|| id.to_owned(), |spec| spec.name(locale).to_owned())
        };
        match (self, locale) {
            (Self::UnknownAction(id), Locale::English) => {
                format!("A saved shortcut names an action this build does not have ({id})")
            }
            (Self::UnknownAction(id), Locale::Chinese) => {
                format!("保存的快捷键指向本版本没有的动作（{id}）")
            }
            (Self::ContextChanged(id), Locale::English) => {
                format!(
                    "A saved shortcut for {} was made in a different place",
                    name(id)
                )
            }
            (Self::ContextChanged(id), Locale::Chinese) => {
                format!("「{}」保存的快捷键来自另一个输入位置", name(id))
            }
            (Self::InvalidKeystroke(keystroke), Locale::English) => {
                format!("{keystroke} is not a key combination this window can bind")
            }
            (Self::InvalidKeystroke(keystroke), Locale::Chinese) => {
                format!("「{keystroke}」不是本窗口能绑定的组合键")
            }
            (
                Self::Conflict {
                    keystroke,
                    first,
                    second,
                },
                Locale::English,
            ) => format!(
                "{} is used by both {} and {}",
                keystroke_label(keystroke, platform),
                name(first),
                name(second)
            ),
            (
                Self::Conflict {
                    keystroke,
                    first,
                    second,
                },
                Locale::Chinese,
            ) => format!(
                "{} 同时用于「{}」和「{}」",
                keystroke_label(keystroke, platform),
                name(first),
                name(second)
            ),
            (Self::NeedsModifier(keystroke), Locale::English) => format!(
                "{} needs Ctrl, Alt or ⌘ so the terminal keeps its own keys",
                keystroke_label(keystroke, platform)
            ),
            (Self::NeedsModifier(keystroke), Locale::Chinese) => format!(
                "{} 必须带上 Ctrl、Alt 或 ⌘，否则终端就收不到自己的按键",
                keystroke_label(keystroke, platform)
            ),
            (Self::UnreadablePreferences, Locale::English) => {
                "The preferences file could not be read, so every shortcut is at its default"
                    .to_owned()
            }
            (Self::UnreadablePreferences, Locale::Chinese) => {
                "偏好文件无法读取，所有快捷键已回到默认".to_owned()
            }
        }
    }
}

/// A keystroke in the canonical spelling this module compares and saves, or `None`
/// when it is not one.
///
/// GPUI writes the platform modifier as `cmd`, `super` or `win` depending on the
/// system it was compiled for. Bindings are saved per platform and compared against
/// each other, so the spelling has to be the same everywhere: `cmd` here, whatever
/// the host, which GPUI parses on every system as the platform modifier.
#[must_use]
pub fn canonical(keystroke: &str) -> Option<String> {
    let parsed = Keystroke::parse(keystroke).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    if parsed.modifiers.function {
        parts.push("fn");
    }
    if parsed.modifiers.control {
        parts.push("ctrl");
    }
    if parsed.modifiers.alt {
        parts.push("alt");
    }
    if parsed.modifiers.platform {
        parts.push("cmd");
    }
    if parsed.modifiers.shift {
        parts.push("shift");
    }
    parts.push(&parsed.key);
    Some(parts.join("-"))
}

/// Whether a combination may be bound to an action at all.
///
/// A binding without Ctrl, Alt or ⌘ would take a key a program needs, which is the
/// one thing the catalogue promises never to do. Function keys are the exception:
/// nothing types them as text.
#[must_use]
pub fn is_bindable(keystroke: &str) -> bool {
    let Some(parsed) = Keystroke::parse(keystroke).ok() else {
        return false;
    };
    let modifiers = &parsed.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
        return true;
    }
    // Escape alone is the window's own "close what floats"; it is a default binding
    // and stays reachable, but a new plain key is not accepted.
    parsed.key == "escape" && !modifiers.shift
}

/// The bindings in force: the catalogue's defaults with the user's changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    platform: Platform,
    /// One entry per [`ACTIONS`] entry, holding that action's keystrokes.
    bindings: Vec<Vec<String>>,
}

impl Keymap {
    /// Every action at its default, which is what a fresh install and a damaged
    /// preferences file both get.
    #[must_use]
    pub fn defaults(platform: Platform) -> Self {
        let bindings = ACTIONS
            .iter()
            .map(|spec| spec.default_keystrokes(platform))
            .collect();
        Self { platform, bindings }
    }

    /// The platform these bindings were made on.
    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The saved bindings laid over the defaults, or the defaults with the reason they
    /// could not be applied.
    ///
    /// A set that names an unknown action, was made in another context, holds a
    /// keystroke this build cannot parse or makes two actions collide is not applied
    /// halfway: the whole set falls back to the defaults and the first problem is
    /// returned so the settings page can say so.
    #[must_use]
    pub fn resolve(
        platform: Platform,
        saved: Option<&KeymapPreferences>,
    ) -> (Self, Option<Problem>) {
        let defaults = Self::defaults(platform);
        let Some(saved) = saved else {
            return (defaults, None);
        };
        let entries = saved.for_platform(platform.id());
        if entries.is_empty() {
            return (defaults, None);
        }
        let mut keymap = defaults.clone();
        for entry in entries {
            let Some(spec) = action(&entry.action) else {
                return (defaults, Some(Problem::UnknownAction(entry.action.clone())));
            };
            if Context::from_id(&entry.context) != Some(spec.context) {
                return (
                    defaults,
                    Some(Problem::ContextChanged(entry.action.clone())),
                );
            }
            let Some(index) = index_of(spec.id) else {
                return (defaults, Some(Problem::UnknownAction(entry.action.clone())));
            };
            match &entry.keystroke {
                None => keymap.bindings[index] = Vec::new(),
                Some(keystroke) => match canonical(keystroke) {
                    Some(canonical) if is_bindable(&canonical) => {
                        keymap.bindings[index] = vec![canonical];
                    }
                    _ => {
                        return (defaults, Some(Problem::InvalidKeystroke(keystroke.clone())));
                    }
                },
            }
        }
        if let Some(conflict) = keymap.first_conflict() {
            return (defaults, Some(conflict));
        }
        (keymap, None)
    }

    /// The saved form of the changes made to the defaults, for this platform.
    #[must_use]
    pub fn to_preferences(&self) -> KeymapPreferences {
        let mut preferences = KeymapPreferences::default();
        preferences.set_for_platform(self.platform.id(), self.overrides());
        preferences
    }

    /// The actions whose bindings differ from their defaults, including the ones the
    /// user unbound.
    #[must_use]
    pub fn overrides(&self) -> Vec<KeymapEntry> {
        let mut entries = Vec::new();
        for (index, spec) in ACTIONS.iter().enumerate() {
            let default = spec.default_keystrokes(self.platform);
            if self.bindings[index] == default {
                continue;
            }
            entries.push(KeymapEntry::new(
                spec.id,
                spec.context.id(),
                self.bindings[index].first().cloned(),
            ));
        }
        entries
    }

    /// How many actions differ from their defaults.
    #[must_use]
    pub fn custom_count(&self) -> usize {
        self.overrides().len()
    }

    /// Every keystroke bound to one action, in the order it was bound.
    #[must_use]
    pub fn keystrokes(&self, id: &str) -> &[String] {
        index_of(id)
            .map(|index| self.bindings[index].as_slice())
            .unwrap_or(&[])
    }

    /// The keystroke a hint should show for an action: the first one bound.
    #[must_use]
    pub fn keystroke(&self, id: &str) -> Option<&str> {
        self.keystrokes(id).first().map(String::as_str)
    }

    /// The shortcut a person reads, such as `Ctrl+Shift+P`; `None` when unbound.
    #[must_use]
    pub fn label(&self, id: &str) -> Option<String> {
        self.keystroke(id)
            .map(|keystroke| keystroke_label(keystroke, self.platform))
    }

    /// Every shortcut of an action as a person reads it.
    #[must_use]
    pub fn labels(&self, id: &str) -> Vec<String> {
        self.keystrokes(id)
            .iter()
            .map(|keystroke| keystroke_label(keystroke, self.platform))
            .collect()
    }

    /// Whether the action still has exactly its default bindings.
    #[must_use]
    pub fn is_default(&self, id: &str) -> bool {
        let Some(spec) = action(id) else {
            return true;
        };
        self.keystrokes(id).iter().map(String::as_str).eq(spec
            .default_keystrokes(self.platform)
            .iter()
            .map(String::as_str))
    }

    /// Whether the action has no shortcut at all.
    #[must_use]
    pub fn is_unbound(&self, id: &str) -> bool {
        self.keystrokes(id).is_empty()
    }

    /// Why `keystroke` cannot be given to `id`, or `None` when it can.
    ///
    /// Only actions whose contexts can be true at the same time collide: the same key
    /// outside a terminal and inside one is two separate bindings, but a shell-wide
    /// binding is resolved before a pane sees the key, so it collides with both.
    #[must_use]
    pub fn problem_with(&self, id: &str, keystroke: &str) -> Option<Problem> {
        let Some(spec) = action(id) else {
            return Some(Problem::UnknownAction(id.to_owned()));
        };
        let Some(candidate) = canonical(keystroke) else {
            return Some(Problem::InvalidKeystroke(keystroke.to_owned()));
        };
        if !is_bindable(&candidate) {
            return Some(Problem::NeedsModifier(candidate));
        }
        for other in ACTIONS {
            if other.id == spec.id || !spec.context.overlaps(other.context) {
                continue;
            }
            if self
                .keystrokes(other.id)
                .iter()
                .any(|bound| bound == &candidate)
            {
                return Some(Problem::Conflict {
                    keystroke: candidate,
                    first: spec.id,
                    second: other.id,
                });
            }
        }
        None
    }

    /// The first pair of actions that answer the same key in overlapping contexts.
    #[must_use]
    pub fn first_conflict(&self) -> Option<Problem> {
        for (index, spec) in ACTIONS.iter().enumerate() {
            for candidate in &self.bindings[index] {
                for other in ACTIONS {
                    if other.id == spec.id || !spec.context.overlaps(other.context) {
                        continue;
                    }
                    if self
                        .keystrokes(other.id)
                        .iter()
                        .any(|bound| bound == candidate)
                    {
                        return Some(Problem::Conflict {
                            keystroke: candidate.clone(),
                            first: spec.id,
                            second: other.id,
                        });
                    }
                }
            }
        }
        None
    }

    /// Gives an action one keystroke, replacing whatever it had.
    ///
    /// # Errors
    ///
    /// Returns why the combination cannot be used; the map is left as it was.
    pub fn set(&mut self, id: &str, keystroke: &str) -> Result<(), Problem> {
        if let Some(problem) = self.problem_with(id, keystroke) {
            return Err(problem);
        }
        let Some(index) = index_of(id) else {
            return Err(Problem::UnknownAction(id.to_owned()));
        };
        let keystroke = canonical(keystroke).unwrap_or_else(|| keystroke.to_owned());
        self.bindings[index] = vec![keystroke];
        Ok(())
    }

    /// Hands an action's keys back, so the terminal program receives them.
    pub fn unbind(&mut self, id: &str) {
        if let Some(index) = index_of(id) {
            self.bindings[index] = Vec::new();
        }
    }

    /// Returns one action to its default bindings.
    pub fn reset(&mut self, id: &str) {
        if let (Some(index), Some(spec)) = (index_of(id), action(id)) {
            self.bindings[index] = spec.default_keystrokes(self.platform);
        }
    }

    /// Returns every action to its default bindings.
    pub fn reset_all(&mut self) {
        *self = Self::defaults(self.platform);
    }

    /// The GPUI bindings of everything currently bound.
    #[must_use]
    pub fn bindings(&self) -> Vec<KeyBinding> {
        let mut bindings = Vec::new();
        for (index, spec) in ACTIONS.iter().enumerate() {
            for keystroke in &self.bindings[index] {
                bindings.push(binding_for(spec, keystroke));
            }
        }
        bindings
    }

    /// The actions the settings page lists for `query`, in catalogue order.
    ///
    /// Every word of the query has to be found in the action's name, its identifier,
    /// its context or one of its shortcuts, so "终端 复制" and "ctrl-shift-c" both
    /// find the same row.
    #[must_use]
    pub fn matching(&self, query: &str, locale: Locale) -> Vec<&'static ActionSpec> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        ACTIONS
            .iter()
            .filter(|spec| {
                if words.is_empty() {
                    return true;
                }
                let name = format!(
                    "{} {}",
                    spec.name(Locale::English),
                    spec.name(Locale::Chinese)
                )
                .to_lowercase();
                let context = spec.context.label(locale).to_lowercase();
                let keys = self.labels(spec.id).join(" ").to_lowercase();
                let keys = format!("{keys} {}", self.keystrokes(spec.id).join(" "));
                words.iter().all(|word| {
                    name.contains(word.as_str())
                        || spec.id.contains(word.as_str())
                        || context.contains(word.as_str())
                        || keys.contains(word.as_str())
                })
            })
            .collect()
    }
}

/// The GPUI binding of one action in one context.
fn binding_for(spec: &'static ActionSpec, keystroke: &str) -> KeyBinding {
    let context = Some(spec.context.predicate());
    match spec.id {
        "nav.local" => KeyBinding::new(keystroke, GoToLocalShells, context),
        "nav.hosts" => KeyBinding::new(keystroke, GoToHosts, context),
        "nav.settings" => KeyBinding::new(keystroke, GoToSettings, context),
        "nav.next" => KeyBinding::new(keystroke, NextDestination, context),
        "nav.previous" => KeyBinding::new(keystroke, PreviousDestination, context),
        "text.size" => KeyBinding::new(keystroke, CycleTextSize, context),
        "shell.new" => KeyBinding::new(keystroke, NewLocalShell, context),
        "language.toggle" => KeyBinding::new(keystroke, ToggleLanguage, context),
        "notice.dismiss" => KeyBinding::new(keystroke, DismissNotice, context),
        "palette.open" => KeyBinding::new(keystroke, OpenPalette, context),
        "tab.close" => KeyBinding::new(keystroke, CloseActiveTab, context),
        "tab.next" => KeyBinding::new(keystroke, NextTab, context),
        "tab.previous" => KeyBinding::new(keystroke, PreviousTab, context),
        "tab.move-left" => KeyBinding::new(keystroke, MoveTabLeft, context),
        "tab.move-right" => KeyBinding::new(keystroke, MoveTabRight, context),
        "tab.select.1" => KeyBinding::new(keystroke, SelectTab1, context),
        "tab.select.2" => KeyBinding::new(keystroke, SelectTab2, context),
        "tab.select.3" => KeyBinding::new(keystroke, SelectTab3, context),
        "tab.select.4" => KeyBinding::new(keystroke, SelectTab4, context),
        "tab.select.5" => KeyBinding::new(keystroke, SelectTab5, context),
        "tab.select.6" => KeyBinding::new(keystroke, SelectTab6, context),
        "tab.select.7" => KeyBinding::new(keystroke, SelectTab7, context),
        "tab.select.8" => KeyBinding::new(keystroke, SelectTab8, context),
        "tab.select.9" => KeyBinding::new(keystroke, SelectTab9, context),
        "sidebar.toggle" => KeyBinding::new(keystroke, ToggleSidebar, context),
        "pane.split-right" => KeyBinding::new(keystroke, SplitPaneRight, context),
        "pane.split-down" => KeyBinding::new(keystroke, SplitPaneDown, context),
        "pane.zoom" => KeyBinding::new(keystroke, ZoomPane, context),
        "pane.focus-left" => KeyBinding::new(keystroke, FocusPaneLeft, context),
        "pane.focus-right" => KeyBinding::new(keystroke, FocusPaneRight, context),
        "pane.focus-up" => KeyBinding::new(keystroke, FocusPaneUp, context),
        "pane.focus-down" => KeyBinding::new(keystroke, FocusPaneDown, context),
        "pane.resize-left" => KeyBinding::new(keystroke, ResizePaneLeft, context),
        "pane.resize-right" => KeyBinding::new(keystroke, ResizePaneRight, context),
        "pane.resize-up" => KeyBinding::new(keystroke, ResizePaneUp, context),
        "pane.resize-down" => KeyBinding::new(keystroke, ResizePaneDown, context),
        "pane.move-left" => KeyBinding::new(keystroke, MovePaneLeft, context),
        "pane.move-right" => KeyBinding::new(keystroke, MovePaneRight, context),
        "pane.move-up" => KeyBinding::new(keystroke, MovePaneUp, context),
        "pane.move-down" => KeyBinding::new(keystroke, MovePaneDown, context),
        "terminal.search" => KeyBinding::new(keystroke, TerminalSearch, context),
        "terminal.copy" => KeyBinding::new(keystroke, TerminalCopy, context),
        "terminal.paste" => KeyBinding::new(keystroke, TerminalPaste, context),
        // The catalogue and this match are kept in step by a test; an unknown
        // identifier would be a programming error, not a user's.
        other => unreachable!("no action is bound under {other}"),
    }
}

/// One keystroke as a person reads it: `Ctrl+Shift+P`, `⇧⌘P`, `Alt+←`.
#[must_use]
pub fn keystroke_label(keystroke: &str, platform: Platform) -> String {
    let (modifiers, key) = split_keystroke(keystroke);
    let key = key_label(key);
    if modifiers.is_empty() {
        return key;
    }
    match platform {
        Platform::MacOs => {
            let mut label = String::new();
            for modifier in ["control", "alt", "shift", "cmd"] {
                if modifiers.contains(&modifier) {
                    label.push_str(match modifier {
                        "control" => "\u{2303}",
                        "alt" => "\u{2325}",
                        "shift" => "\u{21e7}",
                        _ => "\u{2318}",
                    });
                }
            }
            label.push_str(&key);
            label
        }
        Platform::Windows | Platform::Linux => {
            let mut parts: Vec<&str> = Vec::new();
            for modifier in ["control", "alt", "shift", "cmd"] {
                if modifiers.contains(&modifier) {
                    parts.push(match modifier {
                        "control" => "Ctrl",
                        "alt" => "Alt",
                        "shift" => "Shift",
                        _ => "Win",
                    });
                }
            }
            parts.push(&key);
            parts.join("+")
        }
    }
}

/// Splits a keystroke into its modifiers and the key that is pressed.
fn split_keystroke(keystroke: &str) -> (Vec<&str>, &str) {
    let mut modifiers = Vec::new();
    let mut rest = keystroke;
    while let Some((head, tail)) = rest.split_once('-') {
        let name = match head {
            "ctrl" | "control" => "control",
            "alt" => "alt",
            "shift" => "shift",
            "cmd" | "platform" | "super" | "win" => "cmd",
            "fn" | "function" => "function",
            _ => break,
        };
        modifiers.push(name);
        rest = tail;
    }
    (modifiers, rest)
}

/// The readable name of the key itself.
fn key_label(key: &str) -> String {
    match key {
        "escape" => "Esc".to_owned(),
        "pageup" => "PageUp".to_owned(),
        "pagedown" => "PageDown".to_owned(),
        "backspace" => "Backspace".to_owned(),
        "space" => "Space".to_owned(),
        "tab" => "Tab".to_owned(),
        "enter" => "Enter".to_owned(),
        "left" => "\u{2190}".to_owned(),
        "right" => "\u{2192}".to_owned(),
        "up" => "\u{2191}".to_owned(),
        "down" => "\u{2193}".to_owned(),
        single if single.chars().count() == 1 => single.to_uppercase(),
        word => {
            let mut letters = word.chars();
            match letters.next() {
                Some(first) => first.to_uppercase().collect::<String>() + letters.as_str(),
                None => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORMS: [Platform; 3] = [Platform::Windows, Platform::MacOs, Platform::Linux];

    #[test]
    fn every_action_has_a_unique_identifier_and_two_names() {
        let mut ids: Vec<&str> = ACTIONS.iter().map(|spec| spec.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "two actions share an identifier");
        for spec in ACTIONS {
            assert!(
                !spec.name_en.trim().is_empty(),
                "{} has no English name",
                spec.id
            );
            assert!(
                !spec.name_zh.trim().is_empty(),
                "{} has no Chinese name",
                spec.id
            );
            assert_ne!(
                spec.name_en, spec.name_zh,
                "{} is not translated: the Chinese table is a copy",
                spec.id
            );
            assert!(
                spec.name_zh
                    .chars()
                    .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "{} has no Chinese characters",
                spec.id
            );
        }
    }

    #[test]
    fn every_default_is_a_keystroke_this_window_can_bind() {
        for platform in PLATFORMS {
            for spec in ACTIONS {
                for keystroke in spec.default_keystrokes(platform) {
                    assert_eq!(
                        canonical(&keystroke).as_deref(),
                        Some(keystroke.as_str()),
                        "{} on {platform:?} does not parse back: {keystroke}",
                        spec.id
                    );
                    assert!(
                        is_bindable(&keystroke),
                        "{} on {platform:?} would take a typed key: {keystroke}",
                        spec.id
                    );
                }
            }
        }
    }

    #[test]
    fn the_defaults_do_not_collide_with_each_other() {
        for platform in PLATFORMS {
            let keymap = Keymap::defaults(platform);
            assert_eq!(
                keymap.first_conflict(),
                None,
                "the defaults collide on {platform:?}"
            );
        }
    }

    #[test]
    fn every_catalogue_entry_builds_a_binding_and_nothing_else_does() {
        for platform in PLATFORMS {
            let keymap = Keymap::defaults(platform);
            let expected: usize = ACTIONS
                .iter()
                .map(|spec| spec.default_keystrokes(platform).len())
                .sum();
            assert_eq!(keymap.bindings().len(), expected);
            assert!(
                index_of("terminal.search").is_some() && index_of("nope").is_none(),
                "the catalogue is addressable by identifier"
            );
        }
    }

    #[test]
    fn a_saved_binding_replaces_the_default_and_survives_a_round_trip() {
        let mut keymap = Keymap::defaults(Platform::Windows);
        assert!(keymap.is_default("palette.open"));
        keymap
            .set("palette.open", "ctrl-alt-p")
            .expect("a free combination");
        assert_eq!(keymap.keystroke("palette.open"), Some("ctrl-alt-p"));
        assert_eq!(keymap.label("palette.open").as_deref(), Some("Ctrl+Alt+P"));
        assert!(!keymap.is_default("palette.open"));
        assert_eq!(keymap.custom_count(), 1);

        let saved = keymap.to_preferences();
        let (restored, problem) = Keymap::resolve(Platform::Windows, Some(&saved));
        assert_eq!(problem, None);
        assert_eq!(restored, keymap, "a restart keeps the change");
        assert_eq!(restored.keystroke("palette.open"), Some("ctrl-alt-p"));
        assert_eq!(
            restored.keystroke("nav.hosts"),
            Keymap::defaults(Platform::Windows).keystroke("nav.hosts"),
            "the actions the user did not touch keep their defaults"
        );
    }

    #[test]
    fn another_platforms_bindings_are_not_applied() {
        let mut keymap = Keymap::defaults(Platform::MacOs);
        keymap
            .set("palette.open", "cmd-alt-p")
            .expect("a free combination");
        let saved = keymap.to_preferences();
        let (windows, problem) = Keymap::resolve(Platform::Windows, Some(&saved));
        assert_eq!(problem, None);
        assert_eq!(windows, Keymap::defaults(Platform::Windows));
    }

    #[test]
    fn unbinding_hands_the_key_back_and_is_saved_as_an_unbound_action() {
        let mut keymap = Keymap::defaults(Platform::Windows);
        keymap.unbind("terminal.copy");
        assert!(keymap.is_unbound("terminal.copy"));
        assert_eq!(keymap.keystroke("terminal.copy"), None);
        assert_eq!(keymap.label("terminal.copy"), None);

        let saved = keymap.to_preferences();
        assert_eq!(
            saved.windows.len(),
            1,
            "only the changed action is written down"
        );
        assert_eq!(saved.windows[0].action, "terminal.copy");
        assert_eq!(saved.windows[0].keystroke, None);
        let (restored, problem) = Keymap::resolve(Platform::Windows, Some(&saved));
        assert_eq!(problem, None);
        assert!(restored.is_unbound("terminal.copy"));

        keymap.reset("terminal.copy");
        assert!(keymap.is_default("terminal.copy"));
        assert_eq!(keymap.custom_count(), 0);
    }

    #[test]
    fn two_actions_in_one_context_may_not_share_a_key() {
        let mut keymap = Keymap::defaults(Platform::Windows);
        let problem = keymap
            .set("palette.open", "ctrl-shift-w")
            .expect_err("the close-session shortcut is taken");
        match problem {
            Problem::Conflict { first, second, .. } => {
                assert_eq!(first, "palette.open");
                assert_eq!(second, "tab.close");
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(
            keymap.keystroke("palette.open"),
            Keymap::defaults(Platform::Windows).keystroke("palette.open"),
            "a refused change leaves the map alone"
        );
    }

    #[test]
    fn a_shell_wide_binding_collides_with_a_terminal_one_but_the_two_terminal_contexts_do_not() {
        let mut keymap = Keymap::defaults(Platform::Windows);
        // A terminal binding is resolved after the pane takes the keyboard, which is
        // still inside the shell context.
        assert!(matches!(
            keymap.set("terminal.copy", "ctrl-shift-p"),
            Err(Problem::Conflict { .. })
        ));
        // "Outside a terminal" and "in a terminal" cannot both hold, so the same key
        // may be used for one action of each.
        let search = keymap
            .keystroke("terminal.search")
            .expect("the search shortcut")
            .to_owned();
        keymap
            .set("language.toggle", &search)
            .expect("a terminal key does not clash with one used outside a terminal");
        assert_eq!(keymap.keystroke("language.toggle"), Some(search.as_str()));
    }

    #[test]
    fn a_combination_without_a_modifier_is_refused() {
        let mut keymap = Keymap::defaults(Platform::Windows);
        assert!(matches!(
            keymap.set("palette.open", "p"),
            Err(Problem::NeedsModifier(_))
        ));
        assert!(matches!(
            keymap.set("palette.open", "shift-p"),
            Err(Problem::NeedsModifier(_))
        ));
        assert!(matches!(
            keymap.set("palette.open", "not-a-key"),
            Err(Problem::InvalidKeystroke(_))
        ));
    }

    #[test]
    fn a_saved_set_that_names_an_unknown_action_falls_back_and_says_so() {
        let saved = KeymapPreferences {
            windows: vec![KeymapEntry::new(
                "future.action",
                "shell",
                Some("ctrl-alt-k".to_owned()),
            )],
            macos: Vec::new(),
            linux: Vec::new(),
        };
        let (keymap, problem) = Keymap::resolve(Platform::Windows, Some(&saved));
        assert_eq!(keymap, Keymap::defaults(Platform::Windows));
        assert_eq!(
            problem,
            Some(Problem::UnknownAction("future.action".to_owned()))
        );
    }

    #[test]
    fn a_saved_set_from_another_context_or_with_a_bad_key_falls_back() {
        let wrong_context = KeymapPreferences {
            windows: vec![KeymapEntry::new(
                "language.toggle",
                "shell",
                Some("ctrl-alt-k".to_owned()),
            )],
            macos: Vec::new(),
            linux: Vec::new(),
        };
        assert_eq!(
            Keymap::resolve(Platform::Windows, Some(&wrong_context)).1,
            Some(Problem::ContextChanged("language.toggle".to_owned()))
        );

        let bad_key = KeymapPreferences {
            windows: vec![KeymapEntry::new(
                "language.toggle",
                "outside-terminal",
                Some("nope".to_owned()),
            )],
            macos: Vec::new(),
            linux: Vec::new(),
        };
        assert_eq!(
            Keymap::resolve(Platform::Windows, Some(&bad_key)).1,
            Some(Problem::InvalidKeystroke("nope".to_owned()))
        );
    }

    #[test]
    fn a_saved_set_that_collides_is_replaced_by_the_defaults() {
        let saved = KeymapPreferences {
            windows: vec![
                KeymapEntry::new("palette.open", "shell", Some("ctrl-shift-w".to_owned())),
                KeymapEntry::new("tab.close", "shell", Some("ctrl-shift-w".to_owned())),
            ],
            macos: Vec::new(),
            linux: Vec::new(),
        };
        let (keymap, problem) = Keymap::resolve(Platform::Windows, Some(&saved));
        assert_eq!(keymap, Keymap::defaults(Platform::Windows));
        assert!(matches!(problem, Some(Problem::Conflict { .. })));
    }

    #[test]
    fn a_shortcut_reads_the_way_each_platform_writes_it() {
        assert_eq!(
            keystroke_label("ctrl-shift-p", Platform::Windows),
            "Ctrl+Shift+P"
        );
        assert_eq!(keystroke_label("cmd-3", Platform::MacOs), "\u{2318}3");
        assert_eq!(
            keystroke_label("cmd-shift-p", Platform::MacOs),
            "\u{21e7}\u{2318}P"
        );
        assert_eq!(
            keystroke_label("ctrl-pagedown", Platform::Linux),
            "Ctrl+PageDown"
        );
        assert_eq!(
            keystroke_label("alt-left", Platform::Windows),
            "Alt+\u{2190}"
        );
        assert_eq!(
            keystroke_label("alt-shift-=", Platform::Windows),
            "Alt+Shift+="
        );
        assert_eq!(keystroke_label("escape", Platform::Windows), "Esc");
    }

    #[test]
    fn the_settings_search_matches_names_identifiers_contexts_and_keys() {
        let keymap = Keymap::defaults(Platform::Windows);
        let english = keymap.matching("copy", Locale::English);
        assert_eq!(english.len(), 1);
        assert_eq!(english[0].id, "terminal.copy");

        let chinese = keymap.matching("复制", Locale::Chinese);
        assert_eq!(chinese.len(), 1);
        assert_eq!(chinese[0].id, "terminal.copy");

        let by_key = keymap.matching("ctrl-shift-f", Locale::English);
        assert_eq!(by_key[0].id, "terminal.search");

        let in_terminal = keymap.matching("终端内", Locale::Chinese);
        assert_eq!(in_terminal.len(), 3, "the three terminal actions");

        assert_eq!(
            keymap.matching("  ", Locale::English).len(),
            ACTIONS.len(),
            "an empty query lists every action"
        );
        assert!(
            keymap
                .matching("no such action", Locale::English)
                .is_empty()
        );
    }

    #[test]
    fn a_page_and_a_tab_find_their_catalogue_entries() {
        assert_eq!(
            page_action(Destination::Settings).map(|spec| spec.id),
            Some("nav.settings")
        );
        assert_eq!(page_action(Destination::Transfers), None);
        assert_eq!(tab_jump_action(9).map(|spec| spec.id), Some("tab.select.9"));
        assert_eq!(tab_jump_action(10), None);
    }
}
