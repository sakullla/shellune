# 跨功能走查记录（T10）

本文件记录两轮走查：第 1—4 节是早期一轮（其 T10 对 R1、R2、R6、R7、R12）的历史记录，第 5 节是 2026-11-14 全产品精修（T11）的收口记录。当前外壳以 `02-shell-and-design-system.md` 为准。`01-task-flows.md` 是早期调研，`03-first-use-walkthrough.md` 是更早的历史记录；其中“四个入口”“最大字号 115%”“本任务不引入动画”不描述现在的产品。

未在下面标成实测的平台和辅助功能，都是未验证。构建成功、令牌对比度计算和 GPUI 测试窗口不能代替那些项。

## 1. 实现与走查范围

（本节是当时的实现与走查状态：字号还有 200%，窗口下限是 720×480。两者都已在 T4 取消，
现行规则见 `02-shell-and-design-system.md` 的「大文字与窄窗口（720 宽、115%）」。）

五个分区（本地终端、主机、会话、传输、转发）使用 `src/app/theme.rs` 的同一组令牌和 `src/app/components.rs` 的按钮、横幅、状态卡。浅色是按深色 token 查表，不为新页面另建一套颜色。终端里的 ANSI 与真彩色仍由终端程序决定，主题不改写。

文字大小是 90% / 100% / 115% / 200%。默认 100%。从默认走一步仍是 115%，再下一步才是 200%，200% 的下一步回到 90%。中文和英文是同一份 `Strings`；状态卡标题和正文按卡片宽度换行（`whitespace_normal`），不另做字号。

最小窗口仍是 720×480。带标签的侧边栏加上页头主操作放不下时，改用图标栏。这次收起不写入用户的折叠偏好，窗口变宽后标签回来。侧边栏中段可以纵向滚动。展开时，主题、语言、文字大小和动态效果留在栏底。

减少动态效果是应用内的 `Motion`（默认完整）与系统“减少动态效果”设置的合并，任一方要求减少即为减少。完整时，加载状态的圆点做一次约 200ms 的淡入（调试选择器 `state.motion.pulse`，元素 id `state.motion`）。设为减少后，同一个圆点直接以不透明画出（`state.motion.still`），状态卡的标题、原因和下一步仍在。不循环播放。GPUI 0.2.2 不报告系统设置，应用自己读取（Windows `SPI_GETCLIENTAREAANIMATION`、macOS `accessibilityDisplayShouldReduceMotion`、Linux GNOME `enable-animations` / KDE `AnimationDurationFactor`），窗口重新获得焦点时重读；系统要求减少时不改写应用内设置的值，见 `02-shell-and-design-system.md` §2.3。

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

结果：174 passed、0 failed、0 ignored、82 filtered out，耗时 18.25 秒。测试程序是 `C:/Users/12976/.cargo/target/debug/deps/shellune-be14777611f5cc15.exe`。`one_step_of_text_size_is_115_percent_and_stays_inside_the_narrow_window` 断言从默认走一步仍是 `Zoom::Comfortable`（115%），并检查导航项和内容落在最小窗口内。第 2 节的实窗 exe 在这次改名之前构建；改名只动测试函数名，应用行为与那次构建相同。

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

结果：257 passed、0 failed、0 ignored，耗时 18.04 秒。测试程序是 `C:/Users/12976/.cargo/target/debug/deps/shellune-ac8311f8bd553847.exe`。其中 `large_text_file_columns_stay_inside_the_window` 在 200% 下打开文件视图：720×480 与 1120×720 为单栏，路径、刷新、关闭和来源切换落在窗口内，切换后本地与远程路径仍分开；2400×900 才出现双栏，两栏也落在窗口内。这是 GPUI 测试窗口，不是第 2 节那个实窗的逐页点击。

正式交付用的 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo test --workspace --all-features -- --include-ignored` 和 release 构建，本轮没有跑，也不记为通过。

## 4. 未验证

- macOS：未打开窗口。Cmd 快捷键只有代码分支和平台约定测试，未验证。
- Linux：未打开窗口。未验证。
- 屏幕阅读器：应用没有 AccessKit。焦点环、对比度算术和视觉测试都不是屏幕阅读器兼容。未验证。
- 系统减少动态效果：没有读取，应用不跟随。系统里的该项是否影响本窗口：未验证，当前实现也不会去读。
- 实窗逐页走查：浅色/深色切换、200% 文字、中英文长文本、最小窗口拖动、仅键盘完成快速连接和主机编辑，都没有在第 2 节那个实窗里操作。200% 与最小窗口的结论来自 GPUI 测试窗口。
- 指针目标至少 24×24：本轮没有用外部工具在实窗上量控件。未验证。
- 实窗上的对比度取样：本轮没有用外部对比度检查器对运行中的窗口取样。令牌测试见第 3 节，不能写成实窗已测。

## 5. 本轮（T11）全产品精修与真实界面证据

这一节记录 2026-11-14 需求（R1、R2、R3、R4、R5、R6、R8、R11、R24）的收口工作。上面 1—4 节是早前一轮的历史记录：其中的平台读数、字号阶梯与「本轮没有跑门禁」的说法都不描述当前提交。当前外壳与共享控件以 `02-shell-and-design-system.md` 为准。

### 5.1 本轮改了什么

- **连接行有了主连接动作（R2）。** RDP、VNC、串口的记录在主机列表里与 SSH 主机并列，行尾是带字按钮 `hosts.connection.connect.{n}`，双击行同义；两者都走 `connect_saved_connection`，由记录自己的协议决定结果：SSH 打开登录表单，桌面在应用内开一个桌面标签，串口按记录里的设备名打开。编辑 / 复制 / 删除仍是安静图标，点行不会连带编辑或删除。
- **「首次使用」按全部记录判定（R1、R3）。** 主页欢迎卡与主机页首次使用卡都要求 `book.hosts().is_empty() && book.connections().is_empty()`。一本只存了桌面或串口记录的主机文件不再被欢迎卡盖住：之前只看 SSH 主机，那些记录于是既没有列表也没有行可以到达。
- **零结果与空历史各有出口（R3）。** 零结果状态除原因与提示外增加 `hosts.nomatch.clear`：清空搜索词与分组/状态筛选，不删任何记录。最近连接为空时给出 `hosts.recent.all`，一键回到全部主机。
- **桌面全屏有了作用域（R18）。** 全屏只在「会话页、该会话是活动窗格、窗格仍在、标签里只有它这一个叶子」时成立；关掉、切标签、分屏或走到别的页面都由 `settle_desktop_fullscreen` 结束全屏并把窗口还给用户。此前这些路径会把标题与标签条留在隐藏状态，窗口没有可视出口。
- **提示条只画在画面之上（R18）。** 发送失败的提示条 `desktop.notice` 只在有画面时绘制；没有画面时同一句话由状态卡 `desktop.status` 说一次，不再重复。
- **全部标签看板写协议、能直接关闭（R8）。** 看板每行的类型徽标改由会话自己的协议决定（本机 / SSH 远程 / 远程桌面 / 串口，
`kind_badge_parts` 一处给出，命令面板共用同一份映射）：此前只有 local/remote 两面，RDP、VNC 与串口标签会被画成"本地"。
行末新增关闭（`tabs.list.close.{n}`），走顶栏标签同一个入口 `request_close_tab_anchor`，所以传输/转发的确认照旧、只关这一行那个标签。
证据：`the_tab_list_names_each_tab_by_its_protocol_and_closes_only_that_one`（本机 + RDP + 串口三行，逐个徽标断言，点叉号后只剩另外两行且两者仍在运行）。
- **非 SSH 连接行也有会话状态（R2）。** 记录行原来只有名称、协议、目标与分组；现在这个窗口真的持有该记录的桌面/串口会话且已连接时，
名字后出现"已连接"chip（`hosts.connection.open.{n}`，`connection_is_open` 读真实会话）。只保存未启动的记录不写任何状态，
也不写"离线"。证据：`a_saved_desktop_row_only_says_connected_once_a_session_really_runs`。
- **门禁修复（前序遗留）。** `tests/desktop_native.rs` 的格式化、未使用计数与 `chunks_exact`→`as_chunks`；`crates/storage/src/batch.rs` 的 `cloned_ref_to_slice_refs`；`vendor/gpui/src/taffy.rs` 的 f32 字面量回退警告（显式 `0.0_f32` / `1.0_f32`）；T9 的 7 项批次修复按备份补丁 `t9b-repair.patch` 原样应用。

### 5.2 命令与结果

本机 Windows 11，工作区 `C:/Users/12976/project/shellune`，全部命令在此执行；日志与 PNG 都写在仓库外的 `C:/Users/12976/project/_evidence/t11/`。

| 命令 | 结果 |
| --- | --- |
| `cargo fmt --all -- --check` | 通过（退出码 0） |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 通过（退出码 0，日志 `t11-clippy-final.log`） |
| `cargo test --workspace --all-features` | 通过（退出码 0）：32 个测试目标（含 7 个 0 用例的 doctest 目标）、1326 通过、0 失败、1 个 `#[ignore]`；主套件 `unittests src\main.rs` 630 通过（日志 `t11b-test.log`） |
| `python scripts/capture-ui.py --out C:/Users/12976/project/_evidence/t11/ui2` | 53 个场景、200 张 PNG、`failures: []`、`status: ok`（`t11b-capture.log`、`capture-ui-manifest.json`） |

上一轮完整运行里还有几个一次性失败，各自单跑都通过：`tests/ssh_x11.rs`（`"sshd did not start listening on 51186"`）、
`tests/serial_session.rs`（`a_device_that_goes_away_ends_the_session_and_frees_the_port`）、`src/app/tests.rs` 里
`shell.custom was never rendered` 的 6 个。它们出现在本机一次性启动真实 OpenSSH（经 WSL）、串口伪设备与并行负载同时运行时，
本轮没有复现，作为环境事实保留，不写成"从未失败"，也不把它们当成已验证的根因。

其中 `tests/ssh_sftp.rs` 的 `the_directory_a_shell_reports_is_the_one_the_browser_lists` 起初也被算进这一类，**审查复现后
定位到真实原因，不是启动竞态**：这个用例拿系统目录 `/var/log` 当稳定对照，而服务器 `ls -A` 与 SFTP 列举之间会插入瞬态文件
（`wtmp.db-journal` 出现又消失），两次列举因此不同。修法是让用例在账号家目录下自建 `osc7-live` 目录，放四个已知条目（含一个
带空格的名称），`cd` 进去后由 shell 打印 OSC 7，两次列举都比对这个目录，用例结束时删除。这还连带查出第二个来源：等待"shell 第一段输出"的写法会和连接事件同批到达的输出赛跑，
一旦同批就被吞掉，等到 45 秒超时才失败；改为连上即写命令（伪终端会缓存输入），由 OSC 7 报告本身证明命令被执行。这是用例自身的不稳定，不是产品缺陷。

同一个文件里重启服务器的那几个用例还有一处同类竞态：服务器先起在一个空闲端口、停掉、再在另一个更早选定的端口上重启，中间那个端口可能被并行测试占走（`sshd did not start listening on …`）。
本文件因此给启动加了有限次重试（`START_ATTEMPTS`，换端口重试，全部失败才报错并列出每次的原因）；断言没有放宽，这只是不去测"谁先抢到端口"。

**收口这一轮同样的命令得到一次干净的退出码 0（0 失败、1 个 `#[ignore]`），因此本轮按实测记为通过；上面那次竞态作为环境事实保留，
不写成"从未失败"，也不把它的失败当成已验证的根因。**

### 5.3 捕获（本轮）

`python scripts/capture-ui.py --out .../_evidence/t11/ui2`（完整轮，非 quick）：53 个场景、200 张 PNG、`failures: []`、`status: ok`、耗时 644 秒。窗口是隐藏的 GPUI 窗口，读回 GPU 后台缓冲；隔离见清单的 `isolation` 段（每场景独立临时根、凭据只在内存、不拨任何网络）。

| 类别 | 张数 |
| --- | --- |
| `home` | 50 |
| `overlay` | 70 |
| `terminal` | 44 |
| `settings` | 23 |
| `desktop` | 13 |

尺寸矩阵：720×480 有 35 张、1120×720 有 164 张、1680×940 有 1 张；文字大小 100% 172 张、115% 26 张、200% 2 张；语言英文 152 张、中文 48 张；主题深色 141 张、浅色 59 张。

本轮新增 25 张、6 个场景（按 `ui2` 与 `ui-baseline` 两份清单的场景与 PNG 差值）：`hosts-connections-*`（4 张：协议行的名称、徽标、目标与连接按钮，含 720×480）、`home-no-match-*`（4 张：零结果与清除按钮，含 720×480 与中文）、`hosts-read-failed-*`（4 张：主机文件读不出来，含 720×480 与中文）、`desktop-rdp-*`（5 张：RDP 画面与控件条、115% 与中文）、`desktop-vnc-failed-*`（4 张：认证失败卡片，含 720×480 与中文）、`desktop-send-problem-*`（4 张：画面上的提示条，含 115% 与中文）。既有 `home-empty`、`home-hosts`、`hosts-5000`、`hosts-groups`、`hosts-selection`、`hosts-import`、`hosts-connection-*`、`settings*`、`overlay-tabs`、`batch-*`、`files-*`、`transfers-*`、`serial-session`、`terminal-*` 也都在这一轮里重拍。

每张 PNG 的 `evidence_boundary` 写在清单里，页面上也能读到：桌面相关四张只证明这个应用在这些状态下画了什么，不证明真实 RDP/VNC 服务端或真实串口设备。

### 5.4 未验证

- **RDP/VNC 真实服务端**：本机没有可用服务端。`desktop-rdp`、`desktop-vnc-failed`、`desktop-send-problem` 用的是脚本链路（`desktop::scripted`），只证明应用在这些状态下画了什么，不证明真实画面、输入、剪贴板、证书决定或握手。
- **真实串口设备或虚拟串口对**：本机没有。`serial-session` 是显示用窗格，不证明任何设备的收发、流控或拔出。
- **实窗逐页走查**：浅色/深色、200% 文字、中英文长文本、最小窗口拖动以及纯键盘完成一次连接，本轮同样只在 GPUI 测试窗口与捕获里验证，没有在那个真实窗口里逐页操作。
- **macOS / Linux**：未打开窗口。交叉 `cargo check` 只说明代码在那些目标上能编译。
- **屏幕阅读器**：应用没有 AccessKit；焦点环、对比度算术与视觉测试都不等于屏幕阅读器兼容。

