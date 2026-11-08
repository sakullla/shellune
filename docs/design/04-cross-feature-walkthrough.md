# 跨功能走查记录（T10）

本文件记录 T10 对 R1、R2、R6、R7、R12 的走查结果。当前外壳以 `02-shell-and-design-system.md` 为准。`01-task-flows.md` 是早期调研，`03-first-use-walkthrough.md` 是 T2 与 T11 的历史记录；其中“四个入口”“最大字号 115%”“本任务不引入动画”不描述现在的产品。

未在下面标成实测的平台和辅助功能，都是未验证。构建成功、令牌对比度计算和 GPUI 测试窗口不能代替那些项。

## 1. 实现与走查范围

五个分区（本地终端、主机、会话、传输、转发）使用 `src/app/theme.rs` 的同一组令牌和 `src/app/components.rs` 的按钮、横幅、状态卡。浅色是按深色 token 查表，不为新页面另建一套颜色。终端里的 ANSI 与真彩色仍由终端程序决定，主题不改写。

文字大小是 90% / 100% / 115% / 200%。默认 100%。从默认走一步仍是 115%，再下一步才是 200%，200% 的下一步回到 90%。中文和英文是同一份 `Strings`；状态卡标题和正文按卡片宽度换行（`whitespace_normal`），不另做字号。

最小窗口仍是 720×480。带标签的侧边栏加上页头主操作放不下时，改用图标栏。这次收起不写入用户的折叠偏好，窗口变宽后标签回来。侧边栏中段可以纵向滚动。展开时，主题、语言、文字大小和动态效果留在栏底。

减少动态效果是应用内的 `Motion`，默认完整。完整时，加载状态的圆点做一次约 180ms 的淡入（调试选择器 `state.motion.pulse`，元素 id `state.motion`）。设为减少后，同一个圆点直接以不透明画出（`state.motion.still`），状态卡的标题、原因和下一步仍在。不循环播放。GPUI 0.2.2 没有可读的系统减少动态效果接口，应用不读取、也不跟随系统设置。

展开侧边栏的底部控件 id 是 `motion.control`。图标栏为了高度不放这个按钮，只保留主题、语言和文字大小。200% 文字下的最小窗口会强制使用图标栏，这时用命令面板的「减少动态效果」切换同一设置。搜索入口仍在图标栏里；快捷键仍是 Ctrl+Shift+P（macOS 为 ⇧⌘P）。没有再绑一个全局键，避免占用 Shell。

命令面板、标签菜单和会话菜单记下打开前的焦点，关闭后还回那个字段或终端。点击这些层时，焦点可能先落到页面上；上一帧的字段或终端不会被这次页面焦点盖掉。主机关闭或保存后，下一帧回到打开对话框之前的位置。选中的导航项在持有键盘焦点时也画 accent 环：底色是当前页，环是键盘位置。

文件拖放仍走已有的按钮和键盘路径。终端内的 Esc、Ctrl+L、Ctrl+C 仍交给终端程序。本轮没有改粘贴确认、备用屏幕、本地文件名边界、传输中心、续传、转发关闭确认、端点保存和缺失主机提示。

文件栏首选宽度 340 随文字缩放。两栏只在侧边栏以外放得下两栏缩放宽度，并且还留得下终端（100% 时 192，同样缩放）时并排。否则只显示一个来源，栏宽收到内容区，路径、刷新和关闭留在窗口内。来源切换不合并本地和远程路径。终端列裁切会话栏，避免会话栏盖住这些控件。

## 2. Windows 实测

机器与注册表（2026-11-08 读取）：

- `ProductName` = Windows 10 Pro
- `DisplayVersion` = 26H2
- `CurrentBuild` = 26300
- `UBR` = 9711

调试构建：`cargo build --bin shellune --all-features`，产物 `C:\Users\12976\.cargo\target\debug\shellune.exe`（该次 dev profile 约 1 分 25 秒完成）。编译打印了既有的 dead_code 警告（窗格移动文案、`layout.rs` 的比例收集、`view/panes.rs` 的 `leaf_sessions`、`workspace.rs` 的 `context`）。这不是 clippy 通过，也不是 release 构建。

随后启动该 exe（pid 33248），窗口标题为 `Shellune`，测完后 `Stop-Process`，进程已退出。

| 探针 | 客户区 | 其他 |
| --- | --- | --- |
| 独立进程，清单为 PerMonitorV2 | 1680×1080 | 外框 1702×1093；`GetDpiForWindow` = 144（150%） |
| 未声明 DPI 感知的 PowerShell | 1120×720 | `GetDpiForWindow` 仍是 144 |

1120×720 是未感知进程看到的虚拟化尺寸，与设计默认窗口的逻辑尺寸一致。物理客户区以感知探针的 1680×1080 为准。这次没有在该实窗里点击页面、切换浅色或深色、把文字调到 200%，也没有用键盘走完快速连接或主机编辑。

## 3. 本轮开发测试

命令（改名之后的那一次）：

`cargo test --bin shellune --all-features -- app::tests:: -- --test-threads=8`

结果：174 passed、0 failed、0 ignored、82 filtered out，耗时 18.25 秒。测试程序是 `C:/Users/12976/.cargo/target/debug/deps/shellune-be14777611f5cc15.exe`。`one_step_of_text_size_is_115_percent_and_stays_inside_the_minimum_window` 断言从默认走一步仍是 `Zoom::Comfortable`（115%），并检查导航项和内容落在最小窗口内。第 2 节的实窗 exe 在这次改名之前构建；改名只动测试函数名，应用行为与那次构建相同。

与这次走查直接相关、并包含在上述套件中的用例：

- `two_hundred_percent_chinese_keeps_primary_actions_in_the_minimum_window`：中文、200%、720×480。五个分区的 `header.title` 与 `header.primary` 在窗口内，`content` 不超出宽度，侧边栏宽度小于 160（图标栏）。
- `reduced_motion_drops_the_loading_fade_and_keeps_the_state`：在设计尺寸窗口点击 `motion.control` 后，设置为减少；`state.motion.still` 与 `state.card` 都在，`theme::motion_allowed()` 为 false。不把 `state.motion.pulse` 消失当作证据：GPUI 的 `Frame::clear` 不清除 `debug_bounds`，拿掉的选择器会留下旧框。
- `closing_the_palette_returns_focus_to_the_field_that_opened_it`：自定义 Shell 路径获得焦点后打开命令面板，Esc 关闭后面板回到该字段。
- `text_size_steps_through_every_level_and_wraps`：115% 的下一步是 200%（系数 2.0），再下一步是 90%。
- `both_languages_are_complete_and_actually_translated`：两套文案齐全且中文含汉字，其中包括动态效果四条文案。
- 令牌对比度：`body_text_reaches_the_contrast_floor_on_every_surface`、`light_body_text_reaches_the_contrast_floor_on_every_surface`、`light_status_and_accent_pairs_are_readable`、`status_colours_are_readable_and_distinct`、`the_light_terminal_keeps_its_colours_readable`。这是颜色算术，不是实窗取样，也不是屏幕阅读器结果。
- 终端按键：`escape_reaches_the_program_running_in_the_terminal`、`ctrl_l_reaches_the_program_running_in_the_terminal`、`a_ctrl_c_reaches_the_default_shell_that_is_running_a_command`。
- 拖放的非拖放路径：`file_actions_have_a_button_path_and_a_drop_uses_that_same_request`。
- 既有行为仍在同一套件里：多行粘贴确认与取消、备用屏幕、传输中心与续传、转发关闭确认、端点保存、缺失主机说明。

文件栏返修后的命令：

`cargo test --bin shellune --offline -- --test-threads=8`

结果：257 passed、0 failed、0 ignored，耗时 18.04 秒。测试程序是 `C:/Users/12976/.cargo/target/debug/deps/shellune-ac8311f8bd553847.exe`。其中 `two_hundred_percent_file_columns_stay_inside_the_window` 在 200% 下打开文件视图：720×480 与 1120×720 为单栏，路径、刷新、关闭和来源切换落在窗口内，切换后本地与远程路径仍分开；2400×900 才出现双栏，两栏也落在窗口内。这是 GPUI 测试窗口，不是第 2 节那个实窗的逐页点击。

正式交付用的 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features -- --include-ignored` 和 release 构建，本轮没有跑，也不记为通过。

## 4. 未验证

- macOS：未打开窗口。Cmd 快捷键只有代码分支和平台约定测试，未验证。
- Linux：未打开窗口。未验证。
- 屏幕阅读器：应用没有 AccessKit。焦点环、对比度算术和视觉测试都不是屏幕阅读器兼容。未验证。
- 系统减少动态效果：没有读取，应用不跟随。系统里的该项是否影响本窗口：未验证，当前实现也不会去读。
- 实窗逐页走查：浅色/深色切换、200% 文字、中英文长文本、最小窗口拖动、仅键盘完成快速连接和主机编辑，都没有在第 2 节那个实窗里操作。200% 与最小窗口的结论来自 GPUI 测试窗口。
- 指针目标至少 24×24：本轮没有用外部工具在实窗上量控件。未验证。
- 实窗上的对比度取样：本轮没有用外部对比度检查器对运行中的窗口取样。令牌测试见第 3 节，不能写成实窗已测。
