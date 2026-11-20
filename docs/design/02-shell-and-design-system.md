# 应用外壳与设计系统

本文件是 `src/app/theme.rs`、`src/app/components.rs` 与 `src/app/view.rs` 的设计依据（2026-11 重设计：令牌、组件与动效见 §2、§5）。
tokens 只在 `theme.rs` 定义一次，文档与代码不一致时以代码为准并同时修正本文件。

### 当前界面截图入口

普通测试默认生成零张截图。在 Windows 运行 `python scripts/capture-ui.py` 或附加 `--quick`，可生成首页主机列表、本地终端、RDP 画面三张预览。预览使用 1120×720 逻辑窗口、100% 字号、深色英文。

核对单项改动时使用场景 ID。例如，`python scripts/capture-ui.py --only settings --language zh --theme light` 只拍一张浅色中文设置页。`--list` 列出可用场景，`--no-build` 复用已经带有 `ui-capture` 功能且与当前代码一致的构建。下文各轮历史截图数量与变体用于追溯当时的观察。

日常验证可按负责的 crate 或测试名运行。例如，`cargo test --features ui-capture --bin shellune app::capture::` 检查场景选择与像素处理，`python scripts/capture-ui.py --self-test` 检查发布与回滚。CI 的工作区测试一次覆盖远程桌面用例。

## 1. 结构：顶栏标签 + 页面

本次改版取消了侧边栏。用户只在三处之间切换：**主机**（主页）、**终端**（已打开的标签）、**设置**。
本地终端和 SSH 主机是同一类东西，只是连接方式不同，因此合在主机主页里；旧的"会话"页与终端标签
重复，已删除；传输和端口转发是主机/会话的工具，以弹窗打开，不再是菜单项。

```
+----------------------------------------------------------------------------------+
| [◐] [▤ 主机 24] | [● ▤ prod-web ×] [● >_ Bash ×] [+|v]   ……拖动区……   [⌕] [⚙] [_][□][×] |  顶栏 = 标题栏
+----------------------------------------------------------------------------------+
| [◫]          | 全部主机 24  [⌕ 搜索已保存的主机，或输入 user@host:port 直接连接   /] [⏷] [⋯] [+ 新建主机] |  工具条（一行，窄窗口换行）
| ▣ 全部主机 24 |--------------------------------------------------------------------|
| ☆ 收藏      3 | → 连接到 deploy@192.0.2.24 · 不保存 · 打开登录               [回车] |  搜索读成地址时的第一行
| ◷ 最近连接  5 | B1 build-01  deploy@192.0.2.11      Prod  昨天  ●  (☆ ✎ ⧉ 🗑 [连接]) |  一行一台（uniform_list）
| ─────────── | A2 api-02    deploy@192.0.2.24      Prod  2 小时前                  |  工具悬停 / 键盘行才出现
| 分组       + | …                                                                  |
| ▾ ▣ Prod   10 |                                                                    |
|    ▣ DB     2 |                                                                    |
|    ▣ Web    3 |                                                                    |
|   ▣ Staging 4 |                                                                    |
|              |                                                                    |
| >_ 本地终端   |                                                                    |  侧栏底部
+----------------------------------------------------------------------------------+
终端页（标签是头部垂直居中的胶囊：当前标签为 raised 底 + 发丝边，头部只有一条底边）：
| [◐] [▤ 主机 3] ╭● ▣ prod-web ×╮ ● ▣ db … | ‹ ● ▣ api … › [⧉][+|v] ……拖动区…… [⌕] [⚙] [_][□][×] |  头部第一行：标签即身份
| root@…:22  ● X11 关闭            ⫼ ⊟ [终端|文件] [⋯]  | ×                         |  头部第二行：目标、状态、操作
+----------------------------------------------------------------------------------+
| $ …                                                                                |  终端 / 文件管理器
```

- **顶栏即标题栏**（`view/shell.rs::top_bar`、`view/titlebar.rs`）：品牌、主机标签（固定在最左）、
  终端标签、"⧉"（全部标签，只在有标签看不到时出现，见 1.0）、"+"（新建默认 Shell）与 ▾（其他 Shell、快速连接、已保存主机）、拖动区、
  搜索（命令面板）、设置、Windows 窗口按钮。拖动区与窗口按钮都 `occlude`。Windows 上拖动区在按下时由应用发送
  `WM_NCLBUTTONDOWN/HTCAPTION` 交给系统的移动循环（贴靠照常，双击最大化/还原）；三个窗口按钮保留
  系统命中区域（最大化按钮悬停仍有贴靠布局），同时在点击时发送 `WM_SYSCOMMAND`
  （最小化 / 最大化或还原 / 关闭）并停止传播，保证无论系统命中测试是否生效都只执行一次。窗口按钮宽 46、
  高等于顶行、12px 线形图标；悬停为 `surface hover`，按下再深一级 `surface pressed`；关闭按钮悬停为令牌
  `danger solid` + `on danger`，按下为其 82% 不透明度。按钮宽度与图标随显示器缩放、不随应用文字大小，
  所以 720 宽、115% 文字时三个按钮仍在窗口内。macOS 红绿灯是系统画的、不随文字缩放：
  `titlebar::traffic_light_position(metrics)` 按顶行高度算出垂直位置（14pt 按钮在 `header_height` 中居中，
  左缩进 16pt），窗口打开时用偏好文件里的文字大小（`view::saved_zoom`）计算一次——系统只在建窗时接受这个位置，
  运行中改文字大小要到下次打开窗口才重新居中；`traffic_light_room(metrics)` 预留"缩进 + 三个按钮 52pt +
  一个随缩放的间距"，小字号时第一个标签不会滑到红绿灯下面，大字号时也不再按比例放大空白。Linux 保留系统标题栏。
- **主机主页 = 侧栏 + 工具条 + 一张列表（ADR-3，`view/home.rs::home_page`）**。取代下文"仪表盘"各版：
  页面左右两块，右边一块从上到下只有工具条与列表；没有第二个搜索框、卡片带、分段控件与重复计数。
  - **侧栏**（`home.sidebar`，宽 220；`SURFACE` 底 + 右侧发丝线）：顶部是折叠按钮（`home.sidebar.toggle`，图标 `Sidebar`）；
    然后是三个"位置"——全部主机（`hosts.view.all`，计数 = 全部记录）、收藏（`hosts.view.favorites`，计数 = 收藏数）、
    最近连接（`hosts.view.recent`，计数 = 有连接记录的主机数），用 `components::sidebar_item`（选中为 `SURFACE_SELECTED`
    底 + 主文字色 + 500 字重）；分隔线；"分组"标题与右侧 `+`（`hosts.group.new`，打开创建分组对话框）；分组树
    （`hosts.groups` 内每项 `hosts.tree.<n>`，按层级缩进 12，有子分组的项前面有展开箭头 `hosts.tree.<n>.toggle`，叶子
    保留箭头位置以对齐；项本身是 `sidebar_item`：文件夹图标、最后一段名字、命中数）。树里的分组 = 已保存的分组树
    ∪ 任一记录（SSH 主机，以及远程桌面 / 串口记录）写着的路径及其父路径（`hosts.rs::drawn_paths`），所以只被 RDP/VNC/串口
    记录引用的分组也能在侧栏选到；命中数统计所有协议的记录（与该分组的列表条数、删除对话框的数量一致）。**右键分组项**
    打开分组菜单（`hosts.group.menu`：`hosts.group.rename.open` 重命名、`hosts.group.delete.open` 删除，删除仍先选去向）；
    悬停分组项时右端出现 `⋯`（`hosts.tree.<n>.more`，静止时隐藏但保留位置），点它打开同一菜单；当前位置是某个分组时，
    工具条 `⋯` 菜单里也有同样两项（`hosts.more.group.rename`、`hosts.more.group.delete`）。**分组项是键盘停靠点**：
    在搜索框或列表上按 Shift+Tab 进入侧栏（落在当前分组，否则第一个分组），点击分组项也会把焦点给它；获得焦点的项
    画焦点环，↑↓ 在分组间移动，Enter/空格显示该分组，**F2 重命名、Delete（macOS Backspace）删除**（与菜单同一对话框），
    Tab 回到搜索框，Esc 回到列表。树太长时
    在自己的区域里滚动。侧栏底部是**本地终端**（`home.local`），打开与顶栏 ▾ 相同的 Shell 菜单，菜单在条目右侧、
    底边对齐、向上展开（检测到的 Shell、自定义路径、串口一节与新建串口连接）。
  - **位置 = 视图 + 筛选**（`hosts::HomeSection`，`HostsPanel::section/set_section`）：全部 = `HostView::All` 无筛选；
    收藏 = 收藏筛选；最近 = `HostView::Recent`（新到旧，行选择器 `hosts.recent.<n>`）；分组 = 分组筛选（含子分组）。
    四者互斥，由同一组状态读出，侧栏与列表不会不一致；状态筛选、排序和搜索在切换位置时保留。
  - **折叠**：按钮把侧栏收成 56 宽的图标栏（`home.sidebar.folded`，`components::sidebar_icon_item`，名字在工具提示里），
    分组在图标栏上是一个文件夹（`home.sidebar.groups`），点它展开侧栏到分组树。窗口逻辑宽度 < 900（随缩放）时侧栏
    默认折叠，用户按过按钮后以用户选择为准（`ShelluneView::home_sidebar_folded`）。
  - **工具条**（`hosts.toolbar`，`components::toolbar`：最小高 44、底部发丝线、可换行）：页标题（`header.title`，当前位置
    的名字，分组写完整路径；title 字号、最长 240 后省略）与数量（`header.count`，当前列表的条数）；
    **搜索**（`hosts.search`，`components::search_field` 包住 `TextField::set_bare` 的输入框：放大镜、清除按钮、空且
    未聚焦时显示 `/` 键帽；聚焦时整框是 Focused 面 + 焦点环）；筛选按钮（`hosts.filter`，图标 `Filter`，有筛选或排序
    生效时为 accent tint 常亮）打开**筛选菜单**（`hosts.filter.menu`：状态 `hosts.status.any/connected/recent/never` 与排序
    `hosts.sort.saved/name/recent`，当前项打勾，选中即关闭）；`⋯`（`hosts.more`）打开**更多菜单**（`hosts.more.menu`：
    `hosts.import` 导入 SSH 配置、`hosts.batch.open.list` 批量任务；当前位置是分组时另有重命名 / 删除该分组）；最后是唯一的主按钮"新建主机"（`hosts.new`，紧凑高 28）。
    状态或排序不是默认值时，列表上方一行写出（`hosts.filters`：`hosts.filter.status`、`hosts.filter.sort` chip 与
    `hosts.filter.clear`，清除只还原状态与排序，不动当前位置）。
  - **列表**（`hosts.list.<条数>` 内的 `uniform_list` `hosts.rows`，行高 `list_row_height` 52）：每行是
    `components::list_row_frame`（平面行；键盘光标所在行为 Selected 面 + 左侧 accent 竖条；批量选中行为 `ACCENT_TINT` 底）：
    头像（与复选框共用位置，悬停 / 光标 / 已开始多选时换成复选框 `hosts.select.<n>`）、两行文字（名称 + 等宽的
    `user@host:port`，放不下以省略号收尾；缺私钥的主机名称旁有警示 chip）、右侧元信息按列对齐（分组标签列定宽 128、左对齐，
    标签过长以省略号收尾 `hosts.tag.<n>`；相对时间列定宽 88、右对齐 `hosts.last.<n>`，悬停显示 UTC 时间与连接次数；某行
    没有标签或时间时该列留空，其他内容不左移；窗口 < 600（随缩放）时两列在所有行一起省略）、状态点（本窗口有已连接会话时才画绿色点
    `hosts.state.<n>`，没有会话不写"离线"，位置保留以对齐），然后是收藏星（已收藏常显）与悬停 / 光标行才出现的工具：
    编辑、复制（窗口 ≥ 760）、删除、主按钮"连接"（`hosts.connect.<n>`）。端口转发从行上移到右键菜单
    （`host.menu.forwards`）。**单击选中、双击或 Enter 连接、右键打开记录菜单**。其它协议的记录同一种行
    （`hosts.connection.<n>`，名字旁写协议标签，无复选框）。
  - **统一搜索**：输入即过滤（名称、地址、用户、分组，以及主机会拨的 `user@地址:端口`）。文本能读成
    `user@host[:port]`（`hosts::parse_quick_target`）且列表还有记录时，第一行固定为"连接到 user@host:port · 不保存 ·
    打开登录"（`hosts.direct`，没有键盘光标时带"回车"键帽）；Enter 或点击它走快速连接表单同一条临时连接路径
    （登录方式"自动"，不写主机文件，被拒时打开快速连接表单并写明原因），并清空搜索。键盘光标在某行时 Enter 连接该行；
    既没有地址也没有光标时 Enter 连接第一台匹配的主机（`first_searched_host`）。原来的"快速连接"按钮删除，完整的
    快速连接表单仍在顶栏 ▾ 菜单（`menu.quick`）与命令面板里。
  - **空状态**：主机文件里一条记录都没有且没在搜索时，列表位置是欢迎面板（`hosts.empty`，`components::empty_state`：
    新建主机 `hosts.empty.new`、从 SSH 配置导入 `hosts.import.open`、本地终端 `hosts.empty.local`、从备份恢复
    `hosts.empty.restore`）。搜索或状态筛选无结果时是 `hosts.nomatch`：说明 + "清除筛选"（`hosts.nomatch.clear`）；搜索
    读成地址时另给主按钮"连接到 …"（`hosts.nomatch.connect`）。最近为空（`hosts.recent.empty`）、收藏为空
    （`hosts.favorites.empty`，说明"在主机上点星标"）、分组为空（`hosts.group.empty`）各有一句和回到全部主机的按钮。
  - **键盘**（`ShelluneView::hosts_key`，页面或搜索框有焦点、没有对话框或菜单时）：`/`（页面焦点时）与 Ctrl/Cmd+F
    聚焦搜索；↑↓ 移动键盘光标（列表跟随滚动，"连接到 …"行算第 0 行：没有光标即选中它，从第一条记录按 ↑ 回到它，
    此时 Enter——搜索框或页面焦点——直连所输入的地址）；Enter 连接；Ctrl/Cmd+N 新建主机；
    Delete（macOS 的 Backspace）对光标行先问删除确认。Esc 关闭首页菜单。
  - **矮窗口**（高 < 320，随缩放）整页一列滚动，列表保留两行最小高度；720×480 @ 115% 时侧栏为图标栏、工具条换行，
    搜索、新建主机与本地终端都在窗口宽度内。
  - **证据**：`src/app/tests.rs` 的首页测试（`the_home_is_a_sidebar_a_toolbar_and_one_list`、
    `the_sidebar_places_narrow_the_list_and_name_the_page`、`the_sidebar_folds_to_icons_and_opens_again`、
    `an_address_in_the_search_puts_connect_to_on_top_and_a_press_dials_it`、
    `an_address_nobody_saved_is_offered_by_the_empty_list_and_enter_dials_it`、
    `the_keyboard_finds_moves_opens_makes_and_deletes`、
    `two_hundred_hosts_draw_only_their_visible_rows_and_answer_a_search_at_once`、
    `the_filter_menu_narrows_by_status_and_orders_the_list`、`the_more_menu_imports_and_starts_a_batch`、
    `an_empty_book_is_welcomed_with_four_ways_to_start` 等）；`hosts.rs` 单元测试覆盖位置、排序与按地址搜索。
    截图场景：`home-hosts`（24 台）、`home-favorites`、`home-direct`、`home-empty`、`home-no-match`、`home-narrow`、
    `home-collapsed`，深浅、中英各有变体。

  **首页入口新旧对照**（T4）：

  | 入口 | 改版前 | 改版后 |
  | --- | --- | --- |
  | 搜索已保存主机 | 标题行快速连接栏 + 列表工具栏第二个搜索框 | 工具条唯一搜索框（`hosts.search`），`/`、Ctrl/Cmd+F |
  | 直连 `user@host[:port]` | 快速连接栏结果 `home.quick.temporary` | 列表第一行 `hosts.direct` / 无结果时 `hosts.nomatch.connect`，Enter 直连 |
  | 完整快速连接表单 | 标题行"快速连接"按钮 `welcome.quick` | 顶栏 ▾ 菜单 `menu.quick`、命令面板（首页按钮删除） |
  | 本地 Shell | 标题行"本地终端 ▾" `home.local`；快速连接栏可搜到 Shell | 侧栏底部"本地终端" `home.local`（同一菜单）；顶栏 ▾、命令面板 |
  | 最近连接 | 最近连接卡片 `home.recent.*` + 分段"最近连接" | 侧栏"最近连接" `hosts.view.recent` |
  | 收藏 | 收藏卡片 `home.favorite.*` + 工具栏 ☆ 仅收藏 `hosts.filter.favorites` | 侧栏"收藏" `hosts.view.favorites`；行上星标不变 |
  | 分组 | 分组卡片 `home.group.*` + 分段"按分组" + 左侧分组列 / 窄窗口选择条 | 侧栏分组树 `hosts.tree.<n>`；行上分组标签 `hosts.tag.<n>` |
  | 新建 / 重命名 / 删除分组 | 分组列 `+`；列表上方 `hosts.group.rename.open` / `delete.open` 按钮 | 侧栏 `+`；分组项右键菜单或悬停 `⋯`（`hosts.tree.<n>.more`）打开 `hosts.group.menu`；分组项有焦点时 F2 / Delete；分组位置下工具条 `⋯` 菜单 `hosts.more.group.rename` / `.delete` |
  | 状态筛选 | 分组列"状态"一节 / 窄窗口分段控件 | 工具条筛选菜单 `hosts.filter.menu`（另加排序） |
  | 导入 SSH 配置 | 工具栏按钮 `hosts.import` | 工具条 `⋯` 菜单 `hosts.import`；空状态 `hosts.import.open` |
  | 批量任务 | 工具栏按钮 `hosts.batch.open.list` | 工具条 `⋯` 菜单 `hosts.batch.open.list`；选择条 `hosts.batch.open` 不变 |
  | 新建主机 | 标题行主按钮 `hosts.new` | 工具条主按钮 `hosts.new`；Ctrl/Cmd+N |
  | 记录数 | 标题旁 `header.count` + 列表上方"视图 · N / 共 M 台" `hosts.count` | 工具条 `header.count`（当前列表条数），侧栏各位置计数 |
  | 端口转发（单台） | 行上图标 `hosts.forwards.<n>` | 行右键菜单 `host.menu.forwards` |
  | 连接状态 | 行内"当前已连接" chip / 记录行 `hosts.connection.open.<n>` | 状态点 `hosts.state.<n>` / `hosts.connection.state.<n>` |

  **选择器变更（T4，供 T9 汇总）**：删除 `home.quick*`、`home.dashboard`、`home.cards`、`home.recent*`、`home.favorite*`、
  `home.favorites`、`home.group*`、`home.groups`、`home.all`、`welcome.quick`、`hosts.view.groups`、`hosts.count`、
  `hosts.surface`、`hosts.statuses`、`hosts.tree.all`、`hosts.group.chooser`、`hosts.group.option.<名>`、`hosts.group.actions`、
  `hosts.filter.favorites(.chip)`、`hosts.filter.group`、`hosts.group.<n>`（列表分组头）、`hosts.forwards.<n>`；改名
  `hosts.group.tag.<n>` → `hosts.tag.<n>`、`hosts.connection.open.<n>` → `hosts.connection.state.<n>`；新增 `home.sidebar(.folded/.toggle/.groups)`、
  `hosts.view.favorites`、`hosts.filter(.menu/.status/.sort)`、`hosts.sort.*`、`hosts.more(.menu)`、`hosts.more.group.rename/.delete`、`hosts.tree.<n>.more`、`hosts.group.menu`、
  `hosts.direct`、`hosts.nomatch.connect`、`hosts.empty.local`、`hosts.favorites.empty`、`hosts.group.empty`、`hosts.state.<n>`、
  `hosts.last.<n>`（原有，现仅宽窗口）。`hosts.status.*` 只在筛选菜单打开时存在。
  以下"仪表盘"各版说明为历史记录，与上文冲突处以上文为准。
- **主机主页 = 仪表盘（R1，`view/shell.rs::home_page`、`view/home.rs::home_dashboard`）**：从上到下是
  标题行、卡片带、所有主机列表。
  - **标题行**（`home_heading`）：标题 + 记录数（`header.count`）、**快速连接栏**（`home.quick`，全宽 `TextField`，
    放大镜 + 清除）、`快速连接`（`welcome.quick`，打开带密码/私钥的完整表单；窄窗口只剩图标）、
    `本地终端 ▾`（`home.local`）与主按钮 `新建主机`（`hosts.new`）。宽窗口（≥ 900，随缩放）一行排完、栏在中间；
    窄窗口标题与按钮一行、栏独占下一行。
  - **快速连接栏**：一输入就在卡片带的位置列出结果（`home.quick.results`，每行 `home.quick.result.<n>`）：
    先是书里匹配的已保存记录（任何协议，`hosts::quick_matches`，与列表搜索同一个匹配器，名称前缀匹配排前，
    最多 `QUICK_RESULT_LIMIT` = 6 条，`home.quick.saved.<n>`）；文本能读成 `user@host[:port]`
    （`hosts::parse_quick_target`：用户与主机都必填，端口 1–65535，IPv6 字面量不读端口）时再给一条
    "连接到 user@host:port · 不保存"（`home.quick.temporary`），它走快速连接表单同一条临时连接路径
    （`prepare_quick_connection` → `connect_remote`，登录方式为"自动"，清掉表单里上一次输入的密码/私钥，
    不写主机文件，连上后照常可"保存主机"；被拒时打开快速连接表单并写明原因）；最后是名称或程序名包含该词的
    本地 Shell（`home.quick.shell.<index>`，默认 Shell 标"默认"）。↑/↓ 移动高亮、Enter 打开高亮项（带"回车"键帽），
    打开后清空栏、卡片回来；没有任何结果时写一句"没有匹配……输入 user@host[:port] 可直接连接"（`home.quick.empty`）。
  - **本地终端 ▾**：与顶栏"+ ▾"是同一个菜单组件（`shell_menu_overlay`，`shell_menu_home` 区分来源、各自锚点），
    挂在按钮下：检测到的 Shell（选中即新标签打开并记为默认）、`自定义路径…`、"串口"一节列出已保存的串口连接
    （`home.local.serial.<n>`，点即打开该设备）与 `新建串口连接…`（打开连接表单并切到串口协议）。服务器不在这个菜单里
    ——它们就是这一页。首页不再有"本机"启动器一节；新建标签菜单与命令面板里的 Shell 入口不变。
  - **卡片带**（`home.dashboard`，限高：普通窗口为视口高度 40%（72–300，随缩放），矮窗口（< 320 随缩放）一行卡片高，
    超出时带自己滚动，列表因此始终留在窗口里）：依次是 **最近连接**（`home.recent`，`HostBook::recent_hosts` 新到旧，
    卡片写名称、`目标 · 相对时间`，点即连接）、**收藏**（`home.favorites`，`HostBook::favorite_hosts` 按标记顺序，
    任何协议，非 SSH 写协议名，点即连接）、**分组**（`home.groups`，每个顶层分组一张，`hosts::group_overview`：
    名称 + `N 台 · M 在线`（在线读本窗口已连接的会话，不发请求），没有记录的分组没有卡片；点卡片把下方列表筛到该分组、
    展开并滚到列表顶部；矮窗口整页一列滚动时，页面同时滚到列表区）。每区一行：宽窗口按宽度放得下几张就放几张（最小 200、最多 4 张），放不下的由区标题右侧
    `查看全部`（最近 → 列表"最近连接"视图，收藏 → 列表"仅收藏"筛选）接走；窄窗口卡片单列堆叠、每区只放 2 张。
    卡片右键或悬停出现的 `⋯` 打开与列表行相同的右键菜单。某区没有内容就整区不画；三区都空时没有卡片带；
    主机文件为空时显示首次使用卡片（新建主机、从 SSH 配置导入、从备份恢复），不显示卡片带。
    卡片上的目标省略 SSH 默认端口 `:22`，放不下时以单行省略号收尾（`line_clamp(1)` + `text_ellipsis`，
    不用 `truncate()`：gpui 对不换行文本只按完整长度量一次，卡片再收窄时会在半个字符处裁成另一个地址），
    卡片的悬停提示写出完整目标（含端口）和"连接"。
  - **收藏**：持久化在 `hosts.json` 的 `favorites`（T1，`HostBook::set_favorite`/`toggle_favorite`）。三个入口：
    列表行名称右侧的星标（`hosts.favorite.<n>` / 非 SSH 行 `hosts.connection.favorite.<n>`；已收藏时实心 accent
    常驻，未收藏时与行工具一起悬停/键盘光标才出现，点击阻止冒泡）、行/卡片右键菜单第一组的 `加入收藏 / 取消收藏`
    （`host.menu.favorite`）、命令面板里针对"当前对象"的一条（主机页键盘光标所在行，否则当前会话对应的已保存记录；
    `收藏 {名称}` / `取消收藏 {名称}`，只给这一条而不是每台主机一条，搜主机仍只搜到主机）。切换先写文件、写成功才
    采用并弹提示，写失败保留原标记并在主机页写原因。列表工具栏的 ☆（`hosts.filter.favorites`）是"仅收藏"筛选，
    与分组、状态叠加，激活时出现 `仅收藏` chip，"清除筛选"一并去掉。
  - **所有主机**：宽窗口在列表上方写 `所有主机`（`home.all`）；以下列表能力不变。
  - **视觉（新设计语言，结构与行为不变）**：
    - **快速连接栏**：`TextField::set_prominent(true)` 画成主入口。高 40，圆角 12（`XLARGE`），
      `surface` 底加强边，静止 Raised；聚焦时在 Raised 之上加 accent 边与 3px 焦点环。正文字号用 body，图标 16。
      其余输入框不受影响。与所有 `TextField` 一样按 `Metrics::default()` 绘制，不随窗口缩放（115% 时仍是 100% 的尺寸）。
    - **卡片**：最近、收藏、分组三类卡片都由 `home_card_frame` 构成，即共享的 `components::card` 横排
      （surface、弱边、圆角 8、Raised，内边距 12×8）。悬停时即时抬到 Overlay 并换成强边，没有过渡。
      分组卡片的图标块用品牌渐变（accent）配 `ON_ACCENT` 文件夹，与主机卡片的首字母头像区分。
      快速连接的结果面板（`home.quick.results`）也是同一种 `card`。
    - **卡片带的阴影余量**：`home.dashboard` 是滚动框，会裁掉内容的阴影。因此它左右各外扩 8（负外边距），
      再用同样的内边距收回；底部只留 4（Raised 的阴影余量），不收回，但计入限高之内，不额外占用列表的高度。卡片与下方列表左缘仍然对齐，
      静止阴影不会被裁；需要滚动的卡片带在裁切处与“所有主机”仍保持完整的间距。悬停的 Overlay 阴影达不到的部分
      仍会被滚动框裁掉。
    - **进入动画**：卡片依次淡入（`components::enter_in_turn`）。第 n 张卡片的次序为
      “所在区序号 + 区内序号”，每一次序晚 40ms（`TURN_DELAY`，第 6 次序以后不再加），淡入时长为
      `Enter::Rise` 的 BASE 200ms。动画只改透明度，卡片从第一帧起就在最终位置，进入过程中也能按到。
      减少动效时直接画终态。首次使用卡片（`hosts.empty`）用 `theme::enter(…, Enter::Rise)` 进入一次。
    - **列表**：`hosts.surface` 改为同一种 `card`（surface + Raised，内边距 4），不再是 chrome 底。
      行仍是卡片里的平面行：左右内边距 12，名称与地址两行之间留 2。
    - **空状态**：列表的“无匹配”（`hosts.nomatch`）和“暂无最近连接”（`hosts.recent.empty`）
      改用 `components::empty_state`（accent tint 图标块 + 标题 + 可选说明 + 动作），外层保留原选择器。
      首次使用卡片改用 `components::card`，取对话框圆角 12（`XLARGE`）和内边距 24，品牌图标块为 Overlay。
    - **证据**：`home-*` 截图（宽、窄、空、中文；200% 变体已随该档取消）。
      `the_dashboard_cards_fade_in_turn_where_they_stand_and_take_a_press_at_once` 断言进入中与减少动效时
      卡片位置一致、进入中可点；`the_quick_bar_is_the_larger_field_above_the_cards` 断言栏高于列表搜索、
      卡片与列表左对齐。
  - **证据**：截图场景 `home-hosts`（最近 + 分组）、`home-favorites`（收藏卡片与行星标）、`home-quick`（栏的结果）、
    `home-narrow`（860 宽单列）、`home-empty`（首次使用）。fixture 的连接记录是写进文件的、按运行时刻
    取相对时间，不代表真的连过；收藏的标记/取消、快速连接的匹配与临时连接、分组筛选、空区隐藏由 `src/app/tests.rs`
    的仪表盘测试断言。
  以下为仪表盘之前的改版说明，与上文冲突处以上文为准（"本机"启动器一节已删除）。
- **主机主页（改版）**：标题旁直接写记录数（`header.count`），不再有副标题。"服务器"工具栏只有一行：
  搜索、视图分段、☆ 仅收藏、`批量任务` 与 `导入 SSH 配置`（Ghost，窄窗口只剩图标 + 提示）。
  状态筛选在宽窗口挪进左侧分组栏的"状态"一节（`hosts.statuses`，行带状态记号），窄窗口仍是工具栏下的一条分段控件；
  "全部主机 · 3 台中显示 3 台"（`hosts.count`）写在列表上方。列表放在一块 `surface` 底、大圆角的面板里
  （`hosts.surface`）。每行的复选框与头像共享同一个位置：平时显示头像，悬停、键盘光标所在或已开始多选时换成
  复选框；端口转发 / 编辑 / 复制 / 删除四个图标也只在悬停或键盘光标所在行出现，平时每行只剩名称、地址、分组与"连接"。
  已选行是 accent tint 底，不再每行一圈边框。分组视图的分组标题是横跨列表的分隔行，点整行折叠。空结果是面板中央
  的图标 + 标题 + 说明 + 按钮。chip、tag、计数改为柔和药丸，中性 chip 不再画会被读成减号的短横。
  以下为改版前的说明，未提及的行为不变。
- **主机主页**（`home_page`）：标题 + 快速连接栏 / 快速连接 / 本地终端 ▾ / 新建主机；卡片带；"所有主机"是过滤条与
  主机列表。没有主机时显示首次使用卡片（说明主机是什么，提供新建与导入）。
  主机不再画成卡片网格，而是**一行一台的虚拟化列表**（见 1.2 节）。行是页面上的一行而不是一张卡：没有边框，
  悬停抬一级底色，选中是 accent tint 底 + 半透明 accent 边。左侧是真正的复选框（空心方框，选中填 accent 加勾，
  不再借用会被读成"展开"的箭头）、头像、名称与等宽字体地址，最近连接过的补上相对时间
  （"最近连接 3 小时前 · 12 次连接"，悬停显示 UTC 精确时间，`copy::relative_time`）；右侧是分组标签、
  端口转发 / 编辑 / 删除三个安静的图标，最后是带字的 Secondary "连接"按钮——一行唯一的主操作。**双击行即连接**；
  行内按钮都会阻止冒泡，快速连点图标不会顺带打开主机。删除仍先确认。
  搜索框输入关键词后**按 Enter 连接列表最上面那台**（`first_searched_host`），空搜索框按 Enter 什么都不做。
  "保存位置"那一行从主页移走（它在 设置 → 数据 里），主页标题改为 hero 字号 semibold。
  卡片带限高并自身滚动，所有主机列表和它的过滤条因此留在窗口内；矮窗口（720 宽 @ 115%）整页成为一列滚动，
  快速连接栏在最上方、可见，列表过滤条从窗口内开始、滚动整页即可完整到达，列表本身保留两行的最小高度。
- **终端**（`view/shell.rs::session_header`）：顶栏与它下面那行是一个连续头部，会话只写一次（见 1.1 节）。
  只有一个窗格时不画窗格标记与焦点框，分屏按钮在头部第二行；分屏后每个窗格只恢复一条 26 高的标记
  （左端 `窗格 1` / `窗格 2`，活动窗格带 2px accent 竖条、`活动` chip、抬升底色与半透明 accent 外框），
  不再复制会话名称。终端底部状态栏使用 `surface` 底色与顶部分隔线，与终端画布清晰分层。头部第二行里
  "终端 | 文件"是同一连接的两个视图；远程主机的"端口转发"在 `⋯` 菜单里。终端打开（本地启动或远程连
  接成功）时自动切到终端页，键盘交给新终端。
- **设置页**（`view/settings.rs::settings_page`）：**一整页连续滚动**。所有分类（外观、终端、连接、后台提醒、键盘快捷键、
  数据、关于）按顺序排在同一个滚动区域里（`settings.scroll`，每个分类一个 `settings.section.<id>`）。一个分类里
  "一个控件"的设置项合进**同一张卡片**（`settings.group.<分类>`），每项一行：左侧是名称与说明，右侧是控件，
  行间是分隔线，导航当前指向的那一项在行首带 2px accent 竖条（行仍各自带 `settings.page.<item>` /
  `settings.card.<item>`）；快捷键与连接策略这类自带列表的页面仍各自成块。左侧导航是目录：点分类或子项滚到该分类
  （`settings_reveal`，`scroll_to_top_of_item`）；读者自己滚动时，导航高亮跟随页面顶部正在显示的分类
  （`settings_category_in_view`：第一个底边仍在可视区顶部以下的分类）。点击后、滚动生效前，以点击的分类为准
  （`settings_pending_reveal`），滚轮一动即恢复跟随。最后一个分类之后留出约 60% 视口高度的空白，保证任何分类都能
  滚到顶部。只有右侧滚动，导航不动。窗口窄到两栏放不下时，导航折叠为顶部的分类 / 子项选择条，内容仍是同一个
  连续页面。没有标题的设置行不再留一行空白，说明文字与右侧控件垂直居中。偏好保存在 `preferences.json`。
  **视觉（R5，T8 / ADR-7 更新）**：左侧导航不再是带框面板，而是页面左侧一列 `components::nav_item`（宽 200，与首页侧栏
  同尺寸：分类一个控件高 32、圆角 6、16px 图标；当前分类为 selected 底 + 2px accent 竖条（`accent_mark`）+ MEDIUM，
  展开时底色、竖条与焦点环交给当前子项；焦点环只在导航列本身拿到键盘时画：在页面上按 Esc 进入，或（T9 起）在快捷键页以外用方向键移动导航
  （`settings_page_key` 此时把 `nav_focus` 置为 true）；Enter 把键盘交还页面，环随之消失；指针打开的页面不画
  （`the_chosen_settings_entry_is_ringed_only_while_the_column_has_the_keyboard`）；子项不带图标、缩进到分类文字、一个紧凑控件高 28）；分类标题用页标题字号
  （`heading_size`，20）；每组设置是共享的 `components::card`（surface + 弱边 + 圆角 8 + Raised，行自带
  内边距与分隔线）。开 / 关类设置全部是开关（`ShelluneView::switch_control` → `components::switch`）：整块
  （状态词“开 / 关” + 开关）是一个 ≥ 24px 的指针目标，按一下翻转并立即生效，选择器为
  `settings.notice.bell`、`settings.notice.finished`、`settings.keepalive`、`settings.sync.auto`、
  `settings.sync.secrets`、`settings.update.auto`（状态词为 `<选择器>.state`）。多选一仍用分段控件，数值用统一的输入框。
- **弹窗**（`modal`）：新建 / 编辑主机及其桌面 / 串口连接（`hosts.editor`）、连接到主机、
  快速连接、导入 SSH 配置、端口转发、传输、备份、同步冲突、更新确认、删除主机确认（`hosts.confirm`）、关闭会话 /
  关闭应用前的询问（`session.close.question` / `app.close.question`）、快捷键录制（`keys.capture.dialog`）全部是同一个
  `components::modal`：头部是图标块 + 标题 + 关闭按钮（危险问题的图标块用 danger tint），主体滚动，底部
  `components::modal_footer` 右对齐、可换行，按“退路在前、主操作在后”排列——主操作为 Primary 实心，次操作为
  Secondary / Ghost，删除与关闭为 Danger。进入时淡入并从上方 8px 落定（`Enter::Settle`，减少动效时直接出现）。
  关闭时卡片当帧消失，背后的遮罩留下 `components::SCRIM_EXIT`（= `Motion::BASE` 200ms）淡出
  （`components::modal_scrim_exit`，选择器 `modal.scrim.exit`）：gpui 每帧重建元素树，卡片完整淡出要逐个对话框
  保留关闭后的状态，按需求 R5 的确认改为只淡出遮罩。淡出中的遮罩不接收指针，答完即可操作页面；期间另开对话框
  会结束这次淡出；减少动效时遮罩随卡片立即消失。
  点遮罩不关闭（不丢失已填内容），用右上角关闭或 Esc；只有不会丢内容的快捷键录制在点遮罩时取消。删除确认与关闭
  询问原先是页内横幅，现在是对话框：Esc 等于“取消 / 保持运行”，不删除、不停止任何东西。这三个问题打开期间与粘贴
  确认一样接管键盘：除 Esc 外的按键（含 Enter、输入的文字与 Ctrl+W / Ctrl+L 之类已绑定的快捷键）都花在问题上，
  不会作用于遮罩后面看不见的搜索框、终端或页面（`ShelluneView::question_open` + `watch_questions`），Enter 不做
  危险动作，答题只用按钮或 Esc。其余对话框的焦点顺序与 Esc / Enter 语义不变。截图场景：`host-new`、
  `close-question`、`confirm-delete-host`。
- **新建 / 编辑主机**（`view/host_dialog.rs`）：按"要连上一台服务器需要什么"排序——地址 + 端口、用户名、登录方式，
  然后是"名称（可选）"与"分组（可选）"，其余全部折进**高级设置**（跳板机、代理命令、启动命令、文件传输、X11、保活；
  `hosts.advanced`）。折叠头写出里面有什么，用到了任何一项时带"已设置"标记，编辑这样的主机时默认展开
  （`HostDraft::has_advanced`）。焦点落在地址框；在地址里粘贴 `user@host:port` 会拆进用户名与端口
  （`HostDraft::split_address`，只填仍是初始值的字段，IPv6 字面量不拆）；名称留空则以地址命名。
  登录方式四选一：**自动**（默认）、密码、私钥文件、系统 Agent；"带口令的私钥"并入私钥文件下的"这个私钥设置了
  口令"复选框。**自动**（`AuthMethodKind::Auto` / `AuthMethod::Auto`）在一次连接里依次尝试系统 SSH Agent 的全部
  身份、`~/.ssh` 下能免口令打开的 `id_ed25519`/`id_ecdsa`/`id_rsa`、最后是可选的密码；缺什么就跳过什么，全部被拒才
  失败。它的密码可记住在系统凭据库，记住后一次点击即连。
  **跳板主机**（高级设置第一块，`hosts.jump`）：标题下一行说明（未选时是“直接连接……”），已选跳板按经过顺序排成
  带编号的小卡片（`hosts.jump.{i}`），每张有上移 / 下移 / 移除；下面只有一个“+ 添加跳板”（`hosts.jump.add`）。
  按下后在按钮正下方展开选择器（`hosts.jump.picker`，`menu_card` 外观）：顶部搜索框（`hosts.jump.search`，打开即
  获得键盘），下面是候选列表（`hosts.jump.options`，每行 `hosts.jump.option.{i}`：服务器图标、名称、等宽的
  `user@地址:端口`）。候选只取已保存的 **SSH** 主机，去掉正在编辑的这台和已在链中的（`HostDraft::jump_candidates`），
  按名称或地址做不区分大小写的子串过滤，保持保存顺序；没有候选时写出原因（`hosts.jump.none`：全部已在链中 / 没有
  匹配）。键盘：↑↓ 移动高亮（列表跟随滚动），Enter 添加高亮项，Esc 只关闭选择器、不关闭对话框；点击某行同样
  添加。每次添加后选择器保持打开、搜索清空、高亮回到第一项，便于连续添加；点在按钮与选择器之外、再按一次按钮
  或折起高级设置都会关闭它。列表最多显示约 6 行，其余在列表内滚动，所以 24 台或上百台主机时高级设置的高度与
  6 台时相同。选择器在表单流内展开而不是浮在上面：主体是会滚动的对话框，浮层会被主体边缘裁掉；第一次布局时
  主体只滚动到刚好露出整个选择器（比主体高时露出顶部）。保存格式与 `add_jump` / `remove_jump` / `move_jump` 不变。
  测试：`the_jump_picker_narrows_by_name_or_address_and_adds_from_the_keyboard`、
  `twenty_four_hosts_keep_the_jump_section_as_tall_as_six`、
  `the_jump_picker_scrolls_its_list_and_stays_clear_of_the_proxy_field`（选择器与“代理命令”标签、标签与输入框
  互不重叠，720 宽 @115% 与 1120×720、中英文）、`the_jump_picker_scrolls_itself_into_the_smallest_dialog`；
  截图场景 `host-new-jumps`。
- **连接弹窗**：头部只有主机名（进行中时不再在标题下重复地址）。进行中与等待确认时，主体是四步进度
  （连接服务器 → 验证服务器身份 → 登录 → 打开终端，`remote.stage`）：已完成打勾，进行中的一步有呼吸点
  （减少动态效果时静止），等待用户的一步标"等待你操作"并说明在等什么，问题卡片（指纹 / ProxyCommand / X11）
  紧随其后。第一步写出目标地址，第三步写出登录方式（与私钥路径）——只读，进行中不再显示可编辑的表单。
  失败时：先写"下一步怎么做"（本地语言），原始报错作为小号等宽的详情在下面；表单的主按钮变成"重试"
  （`remote.retry`），不再同时出现两个主按钮；不可重试的失败（服务器身份变了）不给表单。X11 转发收在"更多选项…"
  里，主机已设置 X11 时默认展开。进行中的连接可"转到后台"，以标签继续，点标签回到弹窗。
- **端口转发弹窗**：规则卡片写出"类型 · 主机 + 状态"和 `本机地址 → 目标`，右侧启动/停止与编辑、复制、删除
  图标。新建 / 编辑表单：类型分段（本地转发 / 远程转发 / SOCKS 代理，各带一句用途说明）、经由主机、
  带标签的地址与端口（占位是 `127.0.0.1`、`8080`、`db.internal`、`5432` 这类示例）、一句话预览
  （"在本机访问 127.0.0.1:8080，即经由 prod-web 访问 db.internal:5432。"）。从某台主机打开时默认经由该主机。
- **文件管理器（SFTP）**：在头部第二行切到"文件"后占满终端区域：本机 | 服务器两栏并排（窗口窄时分段切换），
  每栏有来源图标与名称、上一级 / 刷新 / 新建文件夹（服务器侧另有上传）、面包屑路径、带表头的列表。
  每个条目一行：图标、名称、大小（KB/MB）、操作图标（下载 / 复制到另一侧 / 重命名 / 删除）；单击文件夹进入，
  文件可拖到另一侧复制。传输进度在底部。
- 只剩一个页面会因为没有终端而空着：终端页没有标签时显示主机主页，主机标签处于选中态。

被否决的候选与理由见 `01-task-flows.md`。R14 排除的是 Telnet、串口、RDP，不是窗格。

### 1.0 顶部与底栏（取代 1.1 的两行头部）

顶部**永远只有一行**：品牌、主机标签、会话标签、全部标签 / 新建、拖动区、搜索、设置、窗口按钮——本地终端与 SSH
完全一致，不再夹带任何会话工具或路径。

**浏览器式标签（R2，`view.rs::browser_tab`、`session_tab`、`tab_strip`，`view/panes.rs::pending_tab`，
`view/shell.rs::home_tab`）。** 三种标签共用同一个外框：

- 标签条是 `chrome` 层；标签是**胶囊**（ADR-7），在头部里垂直居中（高 `Metrics::tab_height` = 34，随缩放；
  主机标签、标签条与滚动条都 `items_center`，相邻标签间距 `HALF`）。当前标签：`RAISED` 底、1px `BORDER` 发丝边、
  圆角 `radius::MEDIUM`、主文字色；其他标签：同样留 1px 透明边（切换当前标签时字不会挪动一个像素）、次要文字色，
  悬停 `surface hover` 浅底与主文字色。头部底边那条发丝线横贯整行，当前标签不再向下伸到页面、也不再取下方页面
  （终端配色或远程画面）的颜色——旧的 `tab_floor`、`desktop_tab_floor`、`DesktopPane::top_edge_colour`、`text_on`
  与相邻标签之间的竖分隔线都已移除，所以任何终端配色或画面下标签的对比都只由本调色板决定（测试
  `the_tabs_are_pills_in_the_middle_of_the_head`：当前标签是 raised 胶囊、在头部中线上、不碰头部底边）。
- 内容依次为：状态点（`components::pulsing_status_dot`；连接中在允许动效时以 `Motion::PULSE` 脉动，减少动效时静止，
  形状与色调仍表达状态）、辨识色竖条（有颜色时）、类型图标（当前标签为 accent）、标题、后台标记（响铃/结束）、关闭按钮。
  **状态文字不在标签里**，只在悬停提示里（名字 · 类型 · 目标 · 状态 · 跳转快捷键 · 标记），所以"连接中""已断开"不会把标签撑宽。
- 标题单行、溢出时以省略号截断，放在一个贴住槽位两边的绝对定位盒里，按槽位实际宽度测量（作为弹性项时
  gpui 会按整段文字测量一次然后只裁不加省略号）。绝对定位的字不参与测量，所以槽位自己带一个宽度：用窗口的
  文字系统按标签字号、当前标签所用的 Medium 字重量出整段标题的宽度（`tab_title_width`，向上取整再加 1px，
  不会被小数像素裁出省略号），作为槽位的 `w`（可收缩到 0、可放大占满剩余）。完整名字在悬停提示与全部标签列表里。
- 关闭按钮是 24×24 以上的指针目标，当前标签上常显，其他标签悬停到标签任意位置时显示（隐藏时仍保留命中区域）。
- 会话标签与待启动标签**随标题变宽**：宽度取内容宽度（状态点、辨识色、图标、量出的标题、标记、关闭按钮与内边距），
  上限 220、下限 120（随缩放，`Metrics::tab_max_width/tab_min_width`，紧凑形态下限 80），可收缩——标签条挤时
  先各自收缩到下限，再横向滚动。所以两个短标题的标签只占自己的字宽，"+"紧跟最后一个标签，不再有固定 220 宽的
  透明尾巴看起来像标签与按钮之间的空隙（测试 `a_tab_is_as_wide_as_its_title_within_the_strips_bounds`、
  `the_tab_list_control_appears_only_when_a_tab_is_out_of_sight`：间隙不超过 `HAIRLINE`）。
  带后台标记的标签最小宽度再加上标记文字的宽度；主机标签固定在最左、只占它的字与计数（最多 220），不进入滚动，
  也不随标签条一起被裁切（它在可裁切的标签条之外，整行最窄也留出它的宽度）。会话标签的类型图标：SSH 为服务器、
  远程桌面为显示器、本地与串口为终端。
- 主机标签与待启动标签同一外形：主机标签没有状态点和关闭按钮；待启动标签的状态点是"未开始"的中性标记，
  "待连接"写在悬停提示里。

**标签溢出时横向滚动。** 会话标签放在 `overflow_x_scroll` + `ScrollHandle`（`tab_scroll`）的条里：收缩到最小宽度后
不再裁切，而是滚动；垂直滚轮也滚动这一行。成为当前的标签（新开、点击、快捷键、全部标签列表）以及条宽变化时，
当前标签会被滚到完全可见并避开两端的渐隐区（`revealed_offset`，在本帧布局后计算、需要时再画一帧），用户自己的滚动
之后不被打断。有标签藏在某一端时，该端出现 48 宽、从 `chrome` 到透明的渐隐与一个滚动按钮
（`session.tabs.scroll.start/end`，每次滚动可见宽度的 3/4，到头为止）；条窄到放不下两个按钮时不画两端，靠"全部标签"。
"全部标签"、新建、搜索、设置与窗口按钮都在滚动条之外，20 个标签时仍在窗口内可点（测试覆盖）。

**"全部标签"按钮只在有标签看不到时出现（R2，`overlays.rs::tab_list_control`、`tab_list_wanted`）。** 条件：
标签条溢出（`strip_overflows`：滚动范围 `max_offset` 超过 `STRIP_ROUNDING_SLACK` = 1.5，直接读滚动范围而不是两端按钮，
条窄到不画两端时也算；每个标签的边都按显示器像素取整，恰好放下的标签可能多出一个设备像素——150% 时 0.67——
这一点不是被藏起来的标签，不能画出滚动按钮与"全部标签"，两端按钮用同一个容差），
或顶栏处于紧凑形态（只显示当前标签），或列表正开着（关闭前不让锚点消失）。所有标签都在条上可见时不画它——它只会
重复标签条，还会站在最后一个标签和"+"之间。位置不变：标签条之后、"+"之前。溢出与否读上一帧布局，变化时由标签条
的布局观察者（与两端渐隐同一个 canvas）再画一帧，所以按钮不会滞后一帧；按钮出现会让标签条更窄（仍溢出），消失会让
它更宽（仍不溢出），两种状态各自稳定，不会来回跳。没有快捷键或命令面板入口；按钮隐藏时每个标签都直接可见，不存在
到达不了的标签。测试 `the_tab_list_control_appears_only_when_a_tab_is_out_of_sight`（2 个短标签不出现、+ 紧跟、
挤窄后出现、紧凑出现）、`twenty_tabs_scroll_and_leave_the_window_buttons_reachable`（20 个标签出现）；只为列表本身的
测试先用 `crowd_the_strip` 把窗口收窄到标签条溢出（保持非紧凑），再点按钮。

**紧凑顶栏（R2、R9，`view/shell.rs::header_compact`）。** 当窗口宽度小于 `HEADER_FULL_ROW`（460）加上每条顶栏
提示一个图标位 `NOTICE_ICON_ROOM`（30）——两者随缩放——再加本行自绘的窗口按钮宽度（Windows 为 3×46，
`titlebar::window_controls_width`）时——115% 字号下窗口窄于约 700 时即属此列——整行放不下品牌、带字与计数的主机标签、一个最小宽度的会话标签和其余控件，
顶栏改画紧凑形态：不画品牌标记（左侧仍留拖动区）；主机标签只画图标（像浏览器的固定标签，字与计数进悬停提示）；
新建控件只留"+"（▾ 里的 Shell 在命令面板与主机主页的"本地终端"菜单里，快速连接在主机主页）；搜索、设置与"+"
取指针目标尺寸（`hit_target`）；拖动区最窄为不随缩放的 24；标签条**收拢**为只显示当前标签（完整、可见，
不滚动、不画两端），其余标签经"全部标签"列表到达。紧凑形态下会话标签与待启动标签不画类型图标（类型在悬停提示里），
把位置留给标题，最小宽度为 `Metrics::tab_compact_min_width`（80，随缩放）。T9 起紧凑形态下标签的关闭按钮也不占行内位置：
它浮在标签右端（标签自身的底色上），只在指针悬停标签时出现，标题因此拿到这块宽度——紧凑形态至少显示名字的
前四个字加省略号（`the_one_compact_tab_shows_four_letters_of_its_name`）；关闭仍可经中键、右键菜单与快捷键。是否紧凑只由窗口宽度、字号与有哪些提示决定，
不读取上一帧布局，所以不会在两种形态之间来回跳。

**顶栏提示（R2、R9，`view/shell.rs::topbar_notice_form`）。** 发现新版本（`topbar.update`）、同步失败或冲突
（`topbar.sync`）、本窗口正在驱动的批量任务（`topbar.batch.active`）三条提示画在拖动区之后、搜索之前，形态由行统一
决定（`NoticeForm`），宽度都已计入，所以提示永远不占当前标签的位置：
- **文字**（`Words`）：窗口不窄于 `HEADER_FULL_ROW + 200×提示数`（随缩放）加窗口按钮、且不窄于
  `TOPBAR_FULL_CHIP_WIDTH`（900，随缩放）时，画成 chip，每条最宽 `NOTICE_WORDS_ROOM`（200，含间距），过长以省略号截断
  （`components::chip_within`），完整句子在悬停提示里；
- **图标**（`Icon`）：其余非紧凑宽度画成 30（随缩放）见方、带提示色调底色的图标（新版本 ↑、同步 ↻、批量 ▶），文字进悬停提示；
- **折叠**（`Folded`）：紧凑顶栏没有任何余量，提示折进已有控件：同步与新版本折进设置按钮，按钮右上角画首条提示
  色调与形状的状态标记，悬停提示追加提示文字，点击先去同步设置、否则去更新卡片；批量任务折进任务中心入口
  （`topbar.batch`），同样画标记，点击回到正在运行的任务（其页面可切到已保存任务）。
测试 `the_top_rows_notices_never_cut_the_current_tab` 在 680–1920 宽、100% 与 115% 下逐一断言三种形态与当前
标签完整；截图场景 `tabs-notices` 覆盖三种形态。

会话的信息与工具在**终端底栏**（`TerminalPane::bar`）。底栏只有左右两侧（ADR-7），随窗口文字大小缩放
（`theme::metrics()`），外框 `overflow_hidden`，任何宽度下都不画出窗格：

- **左侧：会话在哪里**（`terminal.bar.left`，`flex_1`）。状态点、目标（SSH 为 `user@host:port`，本地为 Shell 路径；
  点击复制）、非"已连接"时的状态 chip、非"关闭"的 X11 chip、未保存提示。目标以省略号截断，空间不够时它最先让位，
  但至少留下状态点（40，随缩放）。
- **右侧：会话自己的控件**（`terminal.bar.middle`）。可重连时的"重新连接"图标按钮（`session.reconnect`，↻，悬停提示
  带快捷键）、待处理的主按钮（保存主机 / 无法重连时的"关闭会话"）、终端 | 文件 切换、分屏（仅有终端时），然后是
  搜索（`terminal.search`），最后是会话菜单 `⋯`（`session.more`）。左侧与这些控件由视图交给前台窗格
  （`session_bar_parts` → `TerminalPane::set_session_slot(SessionSlot { left, controls, pinned })`）。
  `controls`（`terminal.bar.controls`，顺序即 `session_bar_controls` 的 `BarControl` 列表）在窄宽时**从末尾起整个
  让位**——放不下的控件换到被一行高度隐藏的第二行；连第一个控件都放不下时整段不画（仍参与布局，测量不变），
  不会被裁成半截；搜索与 `⋯` 在段外、`flex_none`，**任何宽度下搜索与 `⋯` 都在底栏这一行上**。窗格测出让位的控件数
  （`bar_hidden_controls`），会话菜单打开时按同一列表把让位的控件放在菜单最前
  （`session.more.reconnect/save_host/files/terminal/split_right/split_down`，动作与底栏按钮相同）。
- **终端工具都在菜单里。** 底栏不再常驻尺寸（`134×36`）、模式、复制与 A− 字号 A+，也不再按宽度折叠它们（旧的
  `BarFold`、`tools_width`、`session_left_floor`、`chip_reserved_width` 已移除）。`TerminalPane::tool_entries` 生成一组
  菜单项：`terminal.more.size`（只陈述行列）、`.mode`、`.copy`、`.font.decrease/.font.increase/.font.reset`；
  字号步进不关闭菜单，模式与复制执行后关闭（`MenuCloser::Own` 关窗格自己的菜单，`MenuCloser::Window` 关视图画的
  会话菜单）。前台窗格的会话菜单（`view/sessions.rs`，`bar_session()` 即该会话时）在原有项之后接上这组工具；菜单
  最高 `max_h`、超出时纵向滚动（`session.more.scroll`）。没有会话槽位的窗格（分屏里的另一窗格）在自己的底栏上
  只画搜索与它自己的 `terminal.more`（`terminal.more.menu`，同一组工具）。Esc 或点菜单外关闭。测试
  `the_terminal_bar_keeps_files_split_search_and_more_and_its_tools_are_in_the_menu`、
  `a_pane_of_its_own_keeps_search_and_its_menu_with_every_tool_in_it`。

文件视图或没有终端时，左中两组画成独立底栏（`standalone_session_bar`）。"X11 关"是默认状态，不再显示。

**终端状态横幅**（ADR-6、R5，`view/banner.rs`）。会话结束（断开、失败、程序退出）、在终端已经显示过内容之后失败，
或正在重连时，状态不再是叠在最后一屏上的卡片，而是终端**上方**的一行横幅（结束 / 失败时这一行就是
`terminal.failure`；重连中外层是 `terminal.reconnecting`，其中的一行是 `terminal.banner`、下面一条不定进度条；
`terminal.banner` 只在重连中存在，失败横幅外面没有它）：网格整体下移到横幅之下，最后一屏完整可读、可滚动、可选择与复制，
不被任何东西盖住。视图经 `hand_banner` → `TerminalPane::set_status_banner(元素, 焦点)` 交给窗格，窗格把它画在
自己那一列的网格之上（`banner_slot`）。

- **一行**（`banner_line`，高 `large_control_height`，可收缩、`overflow_hidden`）：外框是
  `components::inline_banner_frame(tone, icon)`（色调 chip 底 + 色调 24% 的底边发丝线 + 色调图标），然后是一句话
  （`terminal.failure.headline`：状态词 · 原因，单行省略号截断，完整句子与会话目标在悬停提示里），右侧依次是下一步
  （`terminal.failure.next`：重新连接，或无法重连时的关闭会话；重连中为 `terminal.reconnecting.cancel` 取消）、
  "显示详情 / 收起详情"（`terminal.failure.details`）与关闭（`terminal.failure.dismiss`，悬停提示带 Esc）。
  窗格（或文件视图占位时的窗口）窄于 `BANNER_WORDS_WIDTH`（560，随缩放）时命令改为图标按钮，名字在悬停提示里，
  句子保住这一行的空间；其中“重新连接”用 `components::primary_icon_button`（主按钮的 accent 底与字色），与旁边的
  ghost 图标区分开（T9）。重连中横幅是 warning 色调、刷新图标，行下是一条不定进度条（`progress_bar(Indeterminate)`）。
- **详情**在横幅下方展开（`terminal.failure.detail`）："诊断"标题与"复制诊断"（`terminal.failure.copy`），下面是等宽字的
  完整诊断（`terminal.failure.diagnostic`，`copyable_lines`：每行一个按横幅宽度换行的块，点任何一行复制全部），
  最高 `DIAGNOSTIC_MAX_HEIGHT`（160，随缩放），同时不超过窗格高度的 `DIAGNOSTIC_MAX_SHARE`（40%，外层
  `terminal.failure.room` 以相对高度限制，T9），超出时纵向滚动；720 宽 @115% 展开后网格仍至少留 3 行
  （`unfolded_details_leave_rows_of_the_last_screen_in_a_short_pane`）。展开后长诊断需要多布局一帧（`window.on_next_frame`
  请求重画）。复制的是会话陈述的整段诊断（哪个会话停止 + 原因），经 `sanitize_diagnostic` 清理。
- **键盘**：横幅出现时不抢键盘。终端不再接收输入时，在终端里按 Tab 到达横幅第一个控件；Tab / Shift+Tab 在
  `BannerStop`（下一步、详情、复制、关闭）之间走，走过最后一个回到终端；Enter 或 Space 按下当前控件；Esc 关闭横幅
  （`dismiss_active_banner`，接在视图的 Esc 链上），直到会话陈述别的状态（`banner_closed` 按会话记住关闭时的状态键）。开始重连（`reconnect_session`，远程会话另经
  `keep_reconnect_screen`）时清除该记录：重连中横幅、取消后回到的同一原因、下一次重连都会重新显示，不被上一次的关闭藏住
  （`a_closed_banner_comes_back_after_a_reconnect_is_cancelled`）。
- **重连在最后一屏上进行。** `reconnect_session` 用 `keep_reconnect_screen` 留住旧窗格与失败记录
  （`reconnect_screens`）；新连接建立前，标签里仍画旧窗格的最后一屏，上方是"正在重新连接"横幅与取消
  （`cancel_reconnect`），连接卡片与 `layout_terminal` 在 `connection_behind_screen()` 时让位；新窗格接上
  （`attach_pane` / 桌面接上）或会话关闭时 `forget_reconnect_screen`。重连被拒时横幅回到失败状态。
- 底栏中组同时保留"重新连接"图标按钮（R8 的底栏入口）；横幅关闭后仍可从底栏或会话菜单重连。
- 测试：`a_dropped_session_states_itself_in_a_banner_above_its_last_screen`、
  `the_banner_closes_with_its_button_or_escape_and_the_bar_keeps_reconnect`、`tab_reaches_the_banner_and_enter_presses_its_step`、
  `a_long_diagnostic_unfolds_under_the_banner_scrolls_and_copies`、
  `a_reconnect_runs_under_a_banner_over_the_last_screen_and_can_be_cancelled`，以及 `an_ended_shell_*`（单窗格、左右与上下
  分屏在设计尺寸与 720×480 @115% 下横幅可见、下一步可达；上下分屏 @115% 时窗格比横幅一行还矮，横幅行被压短）。
  截图场景 `terminal-disconnected`、`terminal-banner-failed`、`terminal-banner-details`、`terminal-banner-long`。

**仍用卡片的状态。** 还没有任何屏幕内容的会话——首次连接、证书或密码的问题——仍是居中卡片。
没有终端的会话（连接中且已转到后台、或尚未打开终端）在窗格位置画状态卡片（`components::status_card` 结构）：
连接中用进行中指示条作徽记并说明"连接建立后，终端会在这里打开"（`session_state_card`）。这类卡片的结构
（`status_card_floor`：外层 `overflow_hidden` 居中，内层 `pane.state.scroll` 最高为窗格高、纵向滚动），矮窗格里从
卡片顶部开始、滚轮在卡片上可滚（测试 `a_state_card_in_a_short_pane_starts_at_its_top_and_scrolls_under_the_wheel`）。远程桌面与窄窗口下
铺满的文件视图占着终端的位置，此时卡片画在内容上方居中。所有状态卡与连接卡片（`remote.page.card`）进入时
`Enter::Rise`（BASE），按会话 id 只播放一次；减少动效时直接画在最终位置。

**选择器变更（T6，供 T9 汇总）**：删除 `terminal.status.overlay`、`terminal.status.scroll`（状态卡改为横幅）、底栏常驻工具 `terminal.grid.size`、`terminal.mode`、`terminal.font.decrease/.increase/.reset`（同名动作在菜单里：`terminal.more.size`、`terminal.more.mode`、`terminal.more.copy`、`terminal.more.font.decrease/.increase/.reset`）；新增 `terminal.banner`（仅重连中的横幅行）、`terminal.failure.copy/.detail/.dismiss`、`terminal.reconnecting(.headline/.cancel/.dismiss)`、`session.more.scroll`；保留 `terminal.failure(.headline/.next/.details/.diagnostic)`、`terminal.bar(.left/.middle/.controls/.tools)`、`terminal.search`、`terminal.more(.menu)`、`session.more(.menu)`、`session.tab.<n>`。

**连接在标签里进行。** 按"连接"后标签立即出现并切过去，连接卡片（`connection_page`，与对话框同一份内容
`connection_content`）画在终端区域中央：四步进度、需要回答的问题、失败原因与重试都在这里，不再是盖住主页的
模态框；卡片在屏时它是唯一的操作入口，顶部横幅与底栏不重复重试。只有尚未开始、需要先填凭据时才用对话框。
"转到后台"回到主机页，连接继续在标签里跑。网络类失败（非登录问题，`RemoteFailure::sign_in`）只给"重试"与
"修改登录方式…"，不直接摊开表单。

**可复制。** 界面文字不能拖选，用户会复制的值——地址、路径、指纹、连接失败原因、端口转发映射——点击即复制并
弹"已复制"提示（`components::copy_on_click`，动作 `ShowCopied`）。

### 1.1 单一会话头部（R12）（历史，已由 1.0 取代）

顶部标签与旧会话栏合成**一个连续头部**：两行共用同一 `surface` 底色与同一条底边发丝线
（`session.header`），中间没有第二条带子、没有第二层边框。原因是旧结构把同一个会话写了两遍——
标签写名字，栏里再写一次名字，各配一个关闭按钮，`prod-web` 一个会话就占去两行。

**身份只出现一次。** 名字、辨识色、状态点、后台标记（`响铃`/`结束`）与关闭按钮都只属于标签；
头部第二行（`session.context`）只放标签说不出、又会随着会话变化的东西：开头是会话类型标记（`本地` / `SSH`），
然后是目标（远程是 `user@host:port`，本地是 Shell 程序的完整路径；等宽字体，与名字重复时不画）、非 `已连接` 的状态 chip（`待连接`/`连接中`/`断开`/`结束`）、X11 chip、
未保存提示，以及这一行自己的操作。待连接、连接中、已连接、断开、搜索、文件、分屏都共用这套身份，
切换时名字不跳动、不消失。

**没有消息就不占一行。** 正在运行、没有任何需要提示的会话（已连接、不可重连、不是未保存的快速连接、没有 X11
桥接），且窗口不窄于 `CONTEXT_ONE_LINE_WIDTH` 时，第二行整条收起（`session_fused`）：目标（等宽、muted、截断）
与分屏 / 终端|文件 / `⋯` 作为一组安静的控件放到标签行右侧、搜索与设置之前，头部只有一行，终端拿回这一行。
此时不再画第二个 `×`——标签上的关闭就是会话的关闭。一旦会话有话要说（连接中、断开、可重连、保存主机、X11），
或窗口变窄，第二行就回来，内容与下文一致。

**操作按频率分层。** 第二行常驻：分屏 `⫼` `⊟`、`终端 | 文件` 视图开关、`⋯` 更多、`×` 关闭；
断开后可重连、快速连接未保存时"保存主机"按主按钮留在行上——它们就是用户此刻要看这一行的原因。
低频操作进 `⋯`（`session.more`）：复制会话、端口转发、关闭标签、关闭其他标签。菜单用共享
`menu_card`/`menu_entry`，点遮罩或 Esc 关闭，关闭后键盘回到终端（`dismiss_floating`）。

**分屏只标记窗格。** 分屏后每个窗格顶部是一条 26 高的标记，写 `窗格 1` / `窗格 2` 并在活动窗格上加
2px accent 竖条、`活动` chip 与抬升底色；活动窗格另有半透明 accent 外框，并保留自己的放大与关闭。
标记里没有会话名——那已经在标签上。

**窄窗口与 115% 文字。** 窗口窄于 `CONTEXT_ONE_LINE_WIDTH`（860，随文字缩放）时，关闭留在目标那一行，
操作整组换到第二行内部，头部只长一行、不横向裁切；这是 720×480 + 115% 下最矮的排法。

**状态原因留在终端上方。** X11 失败原因与"保存主机"结果不再住在头部里（一块占窗口 38% 的文本会把头部
撑高），而是画在头部下方、终端之上（`session_notices`），边到边、宽度满格、上限 `X11_NOTICE_SHARE`，
文本可滚动且能用 Page Up/Down 读完。头部因此始终是两行高，终端拿回这段空间。
### 1.2 几千台主机：行模型、分组与批量选择（R4、R8、R18）

服务器管理只有一个视图模型，入口（主页列表、搜索、导入结果、分组树、连接入口、批量选择）都读它，
不各自维护一份过滤结果。

**行模型（`src/app/hosts.rs`）。** 主页把"分组头 + 当前过滤后的主机"压平成 `HostListRow`：

- `HostListRow::Group(HostGroupRow)`：一条分组头，带 `Option<GroupPath>`、层级深度、当前过滤命中的主机数与是否展开；
  `None` 表示"未分组"这个位置（它是一行，不是漏掉的余数）。
- `HostListRow::Host(HostRow)`：只带 `HostId`。行身份永远是主机标识，不是行号：搜索、折叠、重载都不能把选择挪到别的主机。
- `HostList::matched` 是过滤后的主机集合（`matched_len` 即界面上的"当前结果"），`HostList::all` 是全部保存的主机；
  折叠分组只少几行，不改变这两个集合。

分组树同时是"分组头"的来源：`HostsPanel::group_tree` 与 `list` 共用 `push_group`，所以宽窗口的分组列、
窄窗口的分组选择条、按分组视图的分组头三处的数量永远一致。树里会出现只有主机引用、从未被显式创建的分组
（导入或手写的路径），父级由此按路径补齐；重命名/删除这类分支时先把整条分支补进树，再一次写入。

**虚拟化（`src/app/view/home.rs`）。** 列表用 GPUI `uniform_list`，每帧只向它要可见的一段：
5000 台主机是 5000 行数据，不是 5000 个元素。行高取 `Metrics::list_row_height`（随缩放变化）。
每帧被要求绘制的行数记录在 `ShelluneView::hosts_rows_drawn`，测试据此证明"只有可见行被实例化"，
而不是靠读代码下结论。滚动用 `hosts_scroll`（`UniformListScrollHandle`），测试用它把远处的行带进视野。

**过滤与空态。**（T4 起：位置在侧栏，状态与排序在工具条的筛选菜单，见 §1"主机主页"。）列表是平的，
不再有分组头；分组树只画在侧栏。过滤 = 搜索框（名称/地址/用户名/分组，以及 `user@地址:端口`）+ 侧栏位置
（全部主机 / 收藏 / 最近连接 / 某个分组）+ 状态（所有状态 / 当前已连接 / 曾经连接过 / 从未连接）+ 排序
（保存顺序 / 名称 / 最近连接时间，稳定排序）。以下为改版前的写法：过滤条 = 搜索框 + 视图切换 + ☆ 仅收藏 +
状态分段 + 导入按钮，右侧是"视图 · 当前 N / 共 M 台"。
分组过滤是一棵子树（`GroupPath::starts_with`），状态过滤只读窗口已有的数据：
"当前已连接"看本窗口已建立的会话，"曾经连接过 / 从未连接"看主机文件里的重连记录——**任何过滤都不会发起 SSH 请求**。
有过滤时列表上方列出当前条件（分组 chip、仅收藏 chip、状态 chip）和"清除筛选"；结果为空且是过滤造成的时候写
`hosts.nomatch` 与"改搜索词或去掉一个筛选"，空书才写首次使用卡片；最近连接视图为空另有说明。

**宽窄两种形状（R8）。**（T4 起由侧栏的展开 / 图标栏取代，见 §1"主机主页"；以下为改版前。）窗口逻辑宽度 ≥ 900（`GROUP_COLUMN_MIN_WIDTH`）时，左边一列是层级分组树
（缩进、展开箭头、命中数量，点标题即把列表收窄到该子树并展开），右边是列表；窄于 900 时该列不画，
同一批分组变成过滤条里的选择条（"全部主机" + 各顶级分组），判定只有一个谓词 `group_column_fits`。
分组列第一行是"全部主机"（`hosts.tree.all`，计数为全部主机数，清除分组过滤）；"新建分组"是列标题右侧的 `+`。
只有含子分组的节点才画展开箭头，叶子保留箭头的位置以对齐名称；子分组只写最后一段（"Web"，缩进已说明它在"生产"下）。
"未分组"不出现在列里——它不是可以收窄到的分组，列表里的"未分组"标题与"全部主机"都能到达这些主机。

**批量选择（R18）。** 每行左侧的选择框直接改选择集合，选择存 `BTreeSet<HostId>`：跨搜索、跨过滤、跨折叠都成立。
有选择时列表上方出现选择条：已选数量、范围分段（当前结果 / 全部主机）、"全选 / 清空 / 移动主机"。
"当前结果"是过滤后的主机集合（包含被折叠起来的那部分），"全部主机"是整本主机文件，两者数量都直接可从
`HostList` 读出，不会把"当前这一屏"说成全部。删除主机或重载后，不再存在的主机会从选择里移除。

**分组管理（R18）。** 一个弹窗承担创建、重命名、移动所选、删除（`GroupAction` + `GroupEditor`）：
名字与去向都是文本，直到树接受为止；拒绝时把模型的原话写在弹窗里，输入不丢。创建只动树，其它三种经
`GroupService` 一次写入主机与树：

- 重命名把整条分支（含各层子分组）一并改名，分支里的主机跟着改；
- 删除**先选去向**（留空 = 移出分组），受影响数量写在确认正文里，任何一台主机都不会随分组删除；
- 移动把选择集合搬进目标分组（目标不存在则连同父级创建）。

写入成功后才替换窗口内的主机与树，并重算"当前分组过滤是否还存在"（被改名/删除的分组不应继续过滤）。
树读不出来时主机照常列出，原因写在列表上方的横幅里。

**导入结果。** 预览与主页列表是同一套阅读方式：一行一台、带结果标签（可导入 / 需复核 / 被阻止）与逐条原因，
列表在弹窗里滚动，可导入数量写在按钮上；应用后留下的消息列出写入了多少、需复核多少、跳过了多少以及每个
未导入主机的理由。导入不写任何主机，除非按下应用。

预览上方有它自己的搜索（`hosts.import.search`），右侧写"当前显示 N / 共 M 台"：一份配置可以描述几千台主机，
按名称/地址收窄比从头滚到尾实际。行号仍是文件里的位置（`hosts.import.row.{line}`），收窄不会重排身份。
行高随原因条数变化（值得复核的主机要多写几行），因此这里是滚动列表而不是 `uniform_list`；
绘制行数因此有上限 `hosts::IMPORT_ROW_LIMIT`（200），超出的部分由一行文字说明"只绘制前 N 行（共 M 行），
用上面的搜索收窄"（`hosts.import.limited`）。主机列表用真正的等高虚拟化，导入预览用这个上限加搜索，
两者都不为看不见的行建等量元素。

**证据边界。** 捕获场景 `hosts-5000`、`hosts-groups`、`hosts-selection`、`hosts-import` 用的是同一份
确定性 fixture（`hosts::scale_host_shape`；捕获侧额外写入固定标识，因为行头按标识取色）。这些 PNG 只能证明
这一帧画了什么：可见行有界、筛选与滚动可用、选择范围与树结构一致，都由 `src/app/tests.rs` 的
5000 台主机测试断言，二者都不能替代真实窗口里的滚动手感与高 DPI 检查。

### 1.3 多步批量任务（R19、R21）

批量任务是"在一个冻结的主机集合上按顺序执行若干命令"，不是工作流：没有条件、变量、跨机依赖、审批或回滚。
界面分两个面，都由同一个模型驱动（`src/app/batch.rs`），渲染在 `src/app/view/batch.rs`。

**入口（R18）。** 三处都能建同一个任务，因为目标一律来自行模型：过滤条右侧的"批量任务"按钮（对当前结果）、
选择条上的"批量任务"按钮（对选择集合），以及选择条里的范围分段。目标 = 选择集合 ∩ 范围；没有选择时，
范围直接决定目标（"当前结果"是过滤后的主机，"全部主机"是整本主机文件）。预览里也带同一个范围分段，改范围会
重新取快照——**确认之后才冻结**。

**预览（确认前零会话、零网络）。** 预览是一张对话框（`batch.dialog`）：

- 目标段先说数量（`batch.targets.count`，"将在 N 台主机上执行这些命令"），再列主机（名称 +
  `user@host:port`），行数上限 `MAX_PREVIEW_ROWS`，超出部分由一行文字说明还剩多少；快照失败的主机
  单独列出原因，不被静默丢弃。
- 命令段一步一框（`batch.step.{n}`），可增删（`batch.step.add`），最后一步不能删；空命令或超长命令由
  `BatchTask` 拒绝，模型原话写在 `batch.problem` 横幅里，输入不丢。
- 并发段（`batch.concurrency`，1 到 `MAX_BATCH_CONCURRENCY`）与失败策略分段（跳过该主机 / 停止调度新主机），
  另有可选任务名。
- 页脚常驻一句"重试只执行尚未完成的部分，不会撤销已经执行的内容"：这是**不宣称回滚**的地方。

预览不持有连接：`BatchRunner::start` 只接受 `ConfirmedBatch`，而 `ConfirmedBatch` 只能由
`BatchTask::confirm` 产生。测试用计数工厂证明"预览期间 0 次 open、确认后每台一次"。

**运行页。** 同一张对话框换成 `batch.run`，从上到下：

1. 状态 chip + 进度（"已结束 D/T 台 · R 台运行中 · 并发 C"，失败数为正时追加危险 chip）；
2. 状态横幅（`batch.run.explanation`）：暂停写"不再开始新步骤，正在执行的命令继续"，取消中写
   "已请求停止，远端命令尚未退出"——**暂停不等于停止，取消中不等于已取消**；
3. 控制：暂停/继续（`batch.run.pause|resume`）、取消（`batch.run.cancel`，取消中时按钮文字随之变成
   "取消中"）、结束后才有"重试失败项"（`batch.run.retry`）与"关闭"（`batch.run.close.action`）；
4. 逐机行（`batch.run.host.{i}`）：主机名、目标、主机状态 chip、逐步结果 chip（状态词 + `exit N`）、
   失败原因或待答问题；点一行把右侧切到该机。等待用户确认的主机只停自己，行内给出问题原文与"接受/拒绝"
   （`batch.run.accept|decline`），答案直接回给 runner，绝不为用户接受主机密钥；
5. 右侧：该机输出（`batch.run.output`，等宽、stderr 用危险色、保留 `MAX_OUTPUT_LINES` 行并说明省略了多少）
   与时间线（`batch.run.timeline.{n}`：步骤命令、状态、`+Ns → +Ms`）。

主机行数上限 `MAX_RUN_ROWS`，超出的部分由一行文字说明；数量本身永远是准确值。

**重试。** 重试不是"再跑一遍"：`BatchTask::retry_after` 只取未成功的主机，并把每台的起点挪到第一个未成功的
步骤，因此已成功的步骤显示为"此前已完成"、且不会再执行。重试仍然回到预览并需要确认，页脚那句"不撤销"照旧。

**Esc 与焦点。** 预览是提问，Esc = 现在不做，关闭预览并交还焦点；运行页在结束前不接受 Esc——把仍在执行的运行
藏起来，等于让人看不到它。

**证据边界。** 捕获场景 `batch-preview`、`batch-running`、`batch-failed`、`batch-paused`、`batch-cancelling`
用的是**真实** `BatchRunner`（真的线程、真的调度、真的状态折叠），只有 transport 是 fixture：命令输出由脚本给出，
需要定格的那一步会被 fixture 的闸门挡住，直到截图之后才放开。因此这些 PNG 能证明这个页面在这些状态下画了什么，
不能证明任何真实 SSH 服务器、认证或主机密钥行为——那些只有 `tests/ssh_batch.rs` 在可用环境下的集成测试能说明。


**另一套协议的记录（R2、R18）。** 主机列表里的行并不都是 SSH：RDP、VNC 和串口的记录与主机并列，只是没有选择框
（选择是批量与分组移动的目标，两者都只针对 SSH）。每行同样只带 `HostId`，名称、协议标签、目标与分组在同一行内可扫，
右侧三个安静图标仍是编辑 / 复制 / 删除。**主操作是行尾的带字按钮**（`hosts.connection.connect.{n}`），行本身的双击
是同一件事；两者的结果由 `connect_saved_connection` 按记录自己的协议决定：SSH 打开登录表单，RDP/VNC 在应用内开一个
桌面标签，串口按记录里的设备名打开。一行只做一次连接，绝不因为点了行而绕开认证、证书或身份确认。
**会话状态只来自真实会话**（`connection_is_open`）：这个窗口真的持有该记录的 Rdp/Vnc/Serial 会话且已连接时，
名字后面才出现“已连接”chip（`hosts.connection.open.{n}`），与主机行同一条规则；只保存未启动的记录不写任何状态，
也不写“离线”，因为空闲不是保存记录拥有的一种状态。

**「首次使用」的定义是没有任何保存记录（R1、R3）。** 主页的欢迎卡片与主机页的首次使用卡片都以
`book.hosts().is_empty() && book.connections().is_empty()` 为准：一本只存了桌面或串口记录的主机文件不是首次使用，
在那上面画欢迎卡片会让这些记录既没有列表也没有行可以到达。零结果筛选仍走 `hosts.nomatch`，与空书分开：一个说
「当前条件没有命中」，一个说「还没有保存任何东西」。

### 1.4 应用内远程桌面与串口会话（R18—R21、R23）

**同一个标签模型。** RDP、VNC 和串口会话就是标签，和终端共用标签条、全部标签列表（`tabs.list`）、会话列表与状态文案；
会话身份是 `SessionKind::{Rdp,Vnc,Serial}` 加 `HostId`，与 SSH 一样由 `SessionId` 和代次路由事件。分屏一个桌面标签时
得到的是**同一协议的待启动窗格**（`split_pending(PaneTarget::rdp|vnc|serial)`），恢复的布局只放窗格、不拨号：
真正拨号只发生在用户点中那个窗格（`connect_leaf` → `open_desktop_leaf` / `open_serial_leaf`）或按下记录行。
非 SSH 会话的能力集里没有文件、批量、转发与 X11，所以这些入口不是被藏起来，而是构造上不可达。

**桌面画面上的浮层。** 桌面窗格把画面、控件条（缩放 -/百分比/+、全屏）和状态卡片叠在同一块画布上。发送失败时的提示条
（`desktop.notice`）只画在**有画面**的时候；没有画面时同一句话由状态卡片（`desktop.status`）说一次，两处不会同时出现
同一句警告。证书确认是独立的卡片（`desktop.certificate`），不会被提示条替代。
全部用共享组件画（R6）：控件条是 `components::floating_bar`（raised + 强边 + 圆角 8 + Overlay，紧凑 Ghost 按钮，全屏带图标，
百分比与按钮之间一条竖线），浮在别人的画面上所以带浮层阴影。

**新视觉（T7，ADR-7）**：
- **状态卡**改为终端位置同一种 `components::status_card`（raised + 强边 + 圆角 12 + Overlay，左侧色调图块 + 标题 + 一行说明，
  经 `status_card_slot` 居中限宽 440、按画出的宽度测量）：连接中图块是加载标记，其余是显示器图标，色调随阶段；标题与说明
  另有选择器 `desktop.status.headline` / `desktop.status.detail`。没有画面时的警告句是卡片里的一行（警告色调的状态记号 + 换行文字）。
- **会话结束**：窗口不再在桌面上方另画一张失败卡片（旧版与桌面自己的状态卡上下叠成两张），而是与终端同一条顶部横幅
  （ADR-6 的 `status_banner`：图标、一句话、重新连接、详情、关闭，选择器仍是 `terminal.failure*`），桌面自己的状态卡留在画面
  位置说“远程桌面会话已结束”和停在哪一步。横幅关闭后不再回落成旧卡片；Tab、Esc 与终端横幅相同：会话结束后桌面画面上按 Tab 进入横幅第一个控件（窗口每帧把
  横幅首个控件的焦点交给 `DesktopPane::set_banner_focus`，在线会话的 Tab 仍发给远端），键盘离开横幅时回到桌面画面
  （`tab_reaches_the_banner_above_an_ended_desktop_and_enter_reconnects`）；Esc 关闭横幅，键盘无论在横幅上还是在画面上，
  关闭后都在桌面画面（`escape_closes_the_banner_above_an_ended_desktop`）。
- **证书确认**是同一种 `status_card`（警告色调、锁图标），标题是证书摘要，指纹以等宽字体**整段换行显示、不加省略号**
  （`desktop.certificate.fingerprint`，点按复制）——它是决定的依据；底部“拒绝”（Danger）在前、“信任”（Primary）在后，右对齐。
- **提示条**（`desktop.notice`）移到画面**左下角**：在顶部时，最小窗口里它会盖住右上角的缩放控件（截图 `desktop-notices`
  720×480 可见）；最宽 `NOTICE_WIDTH` 420（桌面窗格按 `Metrics::default()` 绘制，与改版前一样不随文字大小缩放）。控件条里百分比的宽度是命名常量 `ZOOM_LABEL_WIDTH`。
- 截图：`desktop-rdp`、`desktop-vnc-failed`（横幅 + 状态卡）、`desktop-certificate`（新增）、`desktop-send-problem`、
  `desktop-notices`，各有浅色中文；旧 200% 变体已随该档取消。

**全屏的作用域（R18）。** 全屏不是窗口进入的一种模式，而是「这块画面此刻是不是屏幕上唯一的那块」：它只在
会话页（`Destination::LocalShells` 的画面上）、该会话是活动窗格、窗格仍在、而且它的标签里只有它这一个叶子时成立。
关掉它、切标签、分屏、或用快捷键走到别的页面，都以 `settle_desktop_fullscreen` 结束全屏并把窗口还给用户——
全屏藏起来的标题与标签条是唯一的出口，窗口不能停在没有出口的状态里。`src/app/tests.rs` 的
`full_screen_gives_the_window_back_when_another_page_is_chosen` 与 `..._when_the_desktop_tab_is_split` 各证一条路径。

**剪贴板（远程桌面剪贴板与文件互通 R1—R4、R6）。** 剪贴板按连接开关（`RdpConfig::clipboard` / `VncConfig::clipboard`，
默认开；RDP 与 VNC 表单里都是勾选框 `connection.field.clipboard`），`DesktopTarget` 带着它交给 `NativeLink::open` → `set_clipboard`；关闭时客户端不收不发，窗格也不读本机剪贴板、
不写远端内容。两个方向的格式规则都在 `src/app/desktop_clipboard.rs`，是纯函数，不需要窗口或服务器就能测：

- **本机 → 远端**（`local_offer`）：文本原样交给 `push_clipboard`。**截断语义**：超过 1 MiB 的文本由客户端在界内最后一个
  完整 UTF-8 字符处截断，截断后的文本照常发送，并在会话接收这组内容时回一条 `ClipboardTooLarge(Text)` 通知，不返回错误——
  这改变了 ADR-1 “本机超限返回参数错误”的约定（T2 落地时改为截断）。图片仍按原约定：超过 32 MiB 的图片条目让
  `push_clipboard` 返回 `Argument`，所以窗格在交出去之前先按 32 MiB 过滤。第一张图片同时给 PNG 与 DIB 两种形式（本机是 PNG
  就原样传，其它格式用 `image` crate 解码后重编码；DIB 为 `BITMAPINFOHEADER` + 32 位 `BI_RGB` 自底向上，第四字节在
  `BI_RGB` 里是保留位，半透明只在 PNG 里保留）。单一形式超过 32 MiB 就只丢那一种，两种都超限才丢图片并提示“剪贴板图片超过
  32 MiB，没有传输”；SVG 和解不开的图片静默不传（远端也贴不出来）。文件列表不走剪贴板，文件靠文件夹映射。
- **远端 → 本机**（`local_item`）：一次远端复制的所有格式写成**一个** `ClipboardItem`（T3 的
  `ClipboardItem::new_entries`），文本与图片同时在，由粘贴的程序自己挑；远端给了 PNG 就用 PNG，否则把 DIB 解成 PNG。
  远端的 DIB 可能是 `CF_DIB`（`BITMAPINFOHEADER`），也可能是带 `BITMAPV5HEADER` 的 `CF_DIBV5`（`BI_BITFIELDS`，掩码在
  头里，含 alpha）。`BITMAPV5HEADER` 之后是否再重复三个颜色掩码各家写法不一（Windows 自己合成的重复，浏览器等多数程序
  不重复），而 `image` crate 总是跳过 12 字节；`dib_to_png` 按包长判断——像素数据总在包尾——没有这 12 字节时先补上，
  两种写法都按原样解出（`a_remote_dibv5_pastes_as_the_same_picture_with_its_transparency`）。远端文本超过 1 MiB 时客户端同样
  截断后照常送达并提示“已截断”；远端图片超过 32 MiB 时从这一组里被跳过，其余格式照常送达并提示“没有传输”。RDP 远端
  文本保留 Windows 的 CRLF 换行，原样写入本机。
- **什么时候推送**：窗格拿到键盘（`cx.on_focus_in`）、窗口重新激活且画面仍有焦点（`observe_window_activation`；GPUI
  的焦点事件在窗口失活时也视为失焦，所以重新激活同样触发 focus-in，两条路径由去重合并成一次）、以及按下粘贴快捷键。
  这样远端程序无论用 Ctrl+V 还是右键“粘贴”都拿到本机最新内容。只在会话 **Live**（画面已出现）且有链路时读剪贴板：
  连接中、等证书回答时 `push_clipboard` 回 `Error::State`，排进队列的推送只会在连接卡片上变成一句
  “发送失败”，用户却什么也没做（`focusing_a_session_still_connecting_raises_no_send_warning`）。因此会话变成 Live 时若画面
  持有键盘且窗口是活动窗口，由下一帧绘制补推一次（`offer_when_live`，每个会话一次；新会话 `attach` 时重置）——首次连接
  时在连接过程中就点了画面、以及重连时窗格一直持有键盘，都没有新的 focus-in 事件，靠的是这一次
  （`a_session_still_connecting_is_offered_the_clipboard_when_it_goes_live`）。Windows 上读剪贴板不便宜：只有 `CF_DIB` 的截图
  每读一次都在界面线程上编码成 PNG，所以窗格先问系统的剪贴板序号（`GetClipboardSequenceNumber`），与上次读取时相同就不读
  （远端内容写入本机后也记下写入后的序号）；其它系统 GPUI 不提供这个序号，照常读取再按指纹去重。
- **去重**（`prints` / `already_shared`）：窗格记着最近一次推送或最近一次从远端写入本机的内容的“指纹”（文本把 CRLF
  统一成 LF 后一份、每张图片一份）。RDP 远端文本带 CRLF，本机读回或用户在本机复制的同一段文字可能是 LF，统一换行后才算
  同一份。当前剪贴板的每一部分都在其中时不再推送——Windows 的 `read_from_clipboard` 只返回第一个格式（有文本时只给文本），
  读回来的永远不是写进去的整项，所以按“部分”比较而不是整项比较。推送只由用户动作触发，远端把内容回送过来也只会写一次
  本机，不会形成循环。
- **粘贴快捷键（R4）**：`Ctrl+V`（macOS 为 `⌘V`）先把本机剪贴板排进输入队列，再把快捷键本身发出去，二者同一队列、
  同一顺序。剪贴板关闭或内容已推送过时，只发快捷键，远端程序粘贴它自己剪贴板里的内容。修饰键此前从不转发——GPUI 把
  Ctrl / Alt / Shift 报成修饰键变化而不是按键——所以过去远端收到的是一个光秃秃的 V。现在普通键按下时按本机持有的修饰键
  在远端按下 Ctrl / Alt / Shift（`InputQueue::set_modifier`），本机松开修饰键时（`on_modifiers_changed`）在远端松开；
  单独按下修饰键不发送（Alt 单按会激活远端菜单栏），Windows 键不转发（单按会打开远端开始菜单）。macOS 上 ⌘ 视同 Ctrl，
  所以 ⌘C / ⌘V 到远端就是 Ctrl+C / Ctrl+V。macOS 以外，按着 Windows / Super 键按下的键整个不发给远端——Super 不转发，
  发出去就成了光秃秃的字母（Super+V 会在远端打出 v）——粘贴快捷键也只认 Ctrl+V
  （`a_key_pressed_with_the_super_key_stays_on_this_computer`）。顺带的结果是 Shift+字母在远端成了大写字母。
  注意 RDP 的剪贴板通道与输入通道是两条虚拟通道，服务器不保证二者之间的顺序：焦点推送通常已经在按键之前把内容送到，
  若内容恰好是按 Ctrl+V 那一刻才变的，远端第一次粘贴可能还是旧内容，需实机确认。

**文件夹映射（R5、R6）。** RDP 表单的“映射本机文件夹”是带浏览按钮（`Browse::Directory`）的输入框，留空即关闭；
选择器 `connection.field.drive_folder`，下方一行说明远端看到什么；VNC 表单没有这一项。
它只存在本机记录与 `platforms.<os>` 覆盖层（T1），`DesktopTarget::drive_folder` 交给 `set_drive("Shellune", …)`，
远端“此电脑”里看到名为 Shellune 的磁盘。自研 RDP 客户端在每次连接开始时检查文件夹
（`crates/desktop/src/rdp/drive.rs`：必须是盘符或 UNC 的完整路径、存在、是文件夹、能列出内容、长度在 MAX_PATH 以内），
通过后由同一个客户端在 rdpdr 通道上提供。

**越界拦截机制。** 让服务器只能待在这个文件夹里的唯一一道防线是 `rdp/drive.rs` 的路径解析（`resolve`）：它拒绝 `..`、
绝对路径和 DOS 设备名（CON、NUL、COM1 等），把剩下的相对路径拼到文件夹上后再 `canonicalize`——Windows 上这一步经
`GetFinalPathNameByHandleW` 解析符号链接、junction 与挂载点，文件夹里的一个链接不能把它指向的东西交给服务器——结果必须
仍在文件夹的最终路径之内；尚不存在的名字（新建）改为解析父目录并同样校验。不满足的一律回 `STATUS_ACCESS_DENIED`。
解析与打开之间仍有一个窗口，这一点没有在真实服务器上验证过。窗格与表单不参与拦截，只负责把文件夹交出去、把结果说出来。

**会话通知与原因码。** 客户端的 `Notice` 经 `LinkEvent::Notice` 到窗格，由 `DesktopNotice` 归类成下面几种，每种在每个会话
里**只显示一次**（新会话 `attach` 时清空），各行有自己的选择器：

| 会话通知（`Notice`） | Rust `Notice` | 选择器 `desktop.notice.*` | 文案（英文 / 中文） |
|---|---|---|---|
| `CLIPBOARD_REFUSED` = 1 | `ClipboardRefused` | `clipboard_refused` | This server does not allow the clipboard, so copy and paste stay on each side. / 该服务器不允许剪贴板，复制粘贴不会在本机与远端之间同步。 |
| `CLIPBOARD_READY` = 5 | `ClipboardReady` | —（不显示） | 撤下上面那条：服务器登录完成后剪贴板才就绪，先前的“不允许”不再成立 |
| `CLIPBOARD_LATIN1_ONLY` = 2 | `ClipboardLatin1Only` | `clipboard_latin1` | This server's clipboard only takes Western characters; text with other characters was not sent. / 该服务器的剪贴板只支持西文字符，含其他字符的文本没有发送。 |
| `CLIPBOARD_TOO_LARGE` = 3，detail = 文本 | `ClipboardTooLarge(Text)` | `text_too_large` | Clipboard text over 1 MiB was cut to fit. / 剪贴板文本超过 1 MiB，已截断。（文本已截断并送达） |
| `CLIPBOARD_TOO_LARGE` = 3，detail = PNG / DIB | `ClipboardTooLarge(Png \| Dib)` | `image_too_large` | A clipboard image over 32 MiB was not passed on. / 剪贴板图片超过 32 MiB，没有传输。（图片被跳过） |
| `DRIVE_UNAVAILABLE` = 4，detail = 原因码 | `DriveUnavailable(DriveProblem)` | `drive_unavailable` | The local folder was not mapped ({reason}); the session goes on without it. / 本机文件夹没有映射（{reason}），会话照常进行。 |
| `DRIVE_REFUSED` = 6，detail = NTSTATUS 或 0 | `DriveRefused(DriveRefusal)` | `drive_refused` | The server did not accept the mapped folder; the session goes on without it. / 服务器没有接受映射的文件夹，会话照常进行。 |

`DriveUnavailable` 的原因码（`SD_DRIVE_*` → `DriveProblem` → `{reason}`）：1 `Missing` 文件夹不存在 / it does not exist；
2 `NotAFolder` 该路径是文件 / the path is a file；3 `NotAbsolute` 不是完整路径 / it is not a full path；4 `TooLong` 路径过长
（UTF-16 单元与 UTF-8 字节都按 MAX_PATH 数）/ the path is too long；5 `NoAccess` 没有读取权限 / it cannot be read；6 `NotAdded`
无法建立磁盘 / the drive could not be set up；其它值 `Other` 原因未知 / the reason is unknown。`DriveRefused` 是与之分开的一种
通知：detail 非 0 是服务器以该 NTSTATUS 拒绝了这个设备（`DriveRefusal::Declined`），0 是宽限期内服务器始终没有打开设备重定向
（`DriveRefusal::NotOpened`）；两者文案相同，会话都照常继续。同一种通知按种类去重：文件夹不可用的几种原因只说第一条
（每次连接也只报一次磁盘问题）；文本截断与图片未传说的是两件不同的事，各算一种。`Unknown` 不显示。

有画面时通知与发送失败的句子同在 `desktop.notice` 提示条里，带一个“知道了”按钮（`desktop.notice.dismiss`）收起；没有画面时
同样的句子写在状态卡片里（经 `components::capped_text` 居中限宽换行）。

**构建与分发。** 两个协议、诊断用的名称表与剪贴板转换都在 `crates/desktop` 的 Rust 代码里，构建不需要原生库、补丁或构建期下载：三个平台都是 `cargo build --workspace --release` 之后直接打包，包内只有可执行文件与 `LICENSE`，不再有 `third-party/` 许可证目录。`ring`（SSH 与 TLS 的加密后端）是唯一自带 C 构建步骤的依赖。

**已知限制（需实机确认）。** 本机没有 RDP / VNC 服务器：cliprdr 与 rdpdr 的真实往返、服务器在 `fDisableCdm` 等策略下的回应、
剪贴板通道与输入通道之间的先后，都只由脚本链路和引擎探针覆盖，真实 RDP / VNC 服务器的往返未在本机验证。服务器打开了
设备重定向却始终不回这个磁盘的 Device Reply 时客户端收不到信号，不会提示。越界拦截在解析与打开之间留有一个
竞态窗口，未在真实服务器上验证过。平台限制：Wayland 下本机写不出图片（远端复制的
图片只有文本部分落到本机）；X11 不支持 INCR，超过约 16 MiB 的图片无法提供给本机其它程序；自研 VNC 客户端收到超过 1 MiB 的
服务器剪贴板文本时按上限截断并报 `ClipboardTooLarge`，会话继续。关闭了剪贴板的主机在 0.4.8 及以前的版本上会让整份主机文件 / 同步文件被拒收，需提示其它
设备升级。

**证据边界。** 捕获场景 `desktop-rdp`、`desktop-vnc-failed`、`desktop-send-problem`、`desktop-notices` 与 `serial-session` 用的是
**脚本链路**（`desktop::scripted`）：几个纯色区域、一条脚本化的状态事件、一个脚本化失败，没有任何服务器被拨号。
它们证明这个应用在这些状态下画了什么（控件条、缩放、阶段卡片、提示条的位置与文案），**不证明**任何真实 RDP/VNC 服务端
的画面、输入、剪贴板或握手，也不证明真实串口设备的收发——本机没有这些端点，对应项在捕获清单和交付说明里都记为未验证。
`connection-rdp-edit` 与 `connection-vnc-edit` 打开的是预先写入的 RDP / VNC 记录的编辑表单，只证明剪贴板勾选框、映射文件夹
输入框（含浏览按钮与说明）画在哪里、写了什么，不证明任何映射或剪贴板真的生效。

### 1.5 启动：四个真实阶段与进度（R3，ADR-4，`view/startup.rs`）

启动时窗口先打开、后读数据，首帧就是启动页：canvas 上居中的应用图标与产品名，下面一根细进度条和一行当前阶段说明。
进度条只跟随真实完成的阶段，不靠计时器假装前进。

- **四个阶段**（`StartupStage`，按顺序执行，每个阶段结束都在主线程上报一次 `report_stage`）：

  | # | 阶段 | 读什么 | 在哪执行 | 阶段说明（英 / 中） |
  |---|---|---|---|---|
  | 1 | Settings | `preferences.json`（配色、语言、文字大小、动效、快捷键） | 构造视图时同步读取（首帧要用） | Reading settings and theme / 正在读取设置与主题 |
  | 2 | Hosts | 主机簿 `hosts.json` 与已知主机 `known_hosts.json` | 后台执行器 | Reading hosts and known servers / 正在读取主机簿与已知主机 |
  | 3 | Workspace | 端口转发、分组树、`workspace.json` | 后台执行器 | Reading forwards, groups and workspace / 正在读取转发、分组与工作区 |
  | 4 | Interface | 终端等宽字体解析并量一次字宽（预热）；启动页已画出一帧 | 字体在后台；首帧由 render 记下 | Preparing the interface / 正在准备界面 |

  四个阶段都结束后显示 “Ready / 准备就绪”。读到的结果先存在 `StartupRun` 里，启动结束时才按与同步启动相同的顺序采纳
  （主机 → 分组 → 信任 → 转发 → 布局；先主机后布局，缺失主机的窗格照常标记）。
- **进度条**：第 i 个阶段结束后目标为 i/4（`StartupRun::target`）。条用
  `components::progress_line(id, from, to, duration, still, metrics)`（2px 高、全圆角、accent 填充，组件内选择器
  `progress.line`），从目标改变那一刻**当前画到的位置**（`BarMotion::at`，同一条 ease-out-cubic 曲线算出；T9 起，
  原先是上一阶段的目标值）以 `Motion::BASE` 200ms、ease-out-cubic 缓动到新目标（`BarMotion::retarget`，阶段结束与
  “继续”时调用）；外层宽 200（随缩放，窄窗时不超过可用宽度），`startup.progress`。两个阶段相隔不到 200ms 时（本机常见
  情况，四阶段合计十几毫秒），新的一段从半路接着走，不再向前跳（`the_bar_sets_off_from_where_it_is_drawn`）。
  阶段说明为 caption 字号、次要文字色，每换一个阶段以 `Motion::FAST` 淡入（`startup.stage`）。
- **品牌时刻**：图标从 96% 大小放大到 100%，整组淡入，用时 `BRAND_ENTRANCE` 250ms（`startup.splash.mark`、
  `startup.splash.brand`、`startup.splash.name`）。计时从构造视图时开始；阶段比它慢就等阶段，比它快也不额外等待。
  产品标语不再出现在启动页（保留在“关于”页）。
- **进入首页**：阶段全部完成且品牌时刻已过时进入 Ready。启动页作为一层只读画面（全满的进度条与 “Ready”）在
  `SPLASH_EXIT` 200ms 内淡出（`startup.crossfade`，不接收指针，首页从第一帧就可点击）；同时首页内容区（标签栏下方的
  `destination_panel`）在 `PAGE_ENTRANCE` 300ms 内淡入并从下方 `PAGE_TRAVEL` 8px 上移到位（`theme::arrive`）。
  主机首页分两块错开进入（`ShelluneView::home_entrance` → `theme::arrive_after`）：侧栏先（`startup.page.sidebar`），
  列表晚 `PAGE_STAGGER` 40ms（`startup.page.list`），各自淡入并上移；减少动效时只淡入。
- **减少动效**（用户偏好或系统设置，`Choreography::for_motion`）：只保留淡入淡出——图标不缩放、首页不位移、进度条直接
  跳到目标值；品牌时刻（250ms 淡入）、启动页淡出与首页淡入照常。系统设置要靠辅助程序回答的平台（Linux，
  `SYSTEM_MOTION_QUERY_BLOCKS`）在后台询问，回答到达前只画 canvas 与标题行，最多等 `SYSTEM_MOTION_WAIT`（400ms），
  品牌时刻从回答到达（或超时）时开始计时；超时按用户自己的偏好继续，迟到的回答仍用于之后的帧。Windows 与 macOS 同步读取。
- **阶段失败**：阶段 1 在 `preferences.json` 存在但不是可读的设置文件时失败（默认设置照常生效）；阶段 2、3 在任一文件读取
  失败时失败（原因取存储层给出的第一条）；阶段 4 不会失败（字体缺失时终端本就按比例回退）。失败时启动页停在该阶段：
  品牌下方的进度条与阶段说明整块换成停止面板（`startup_stop_panel`，`startup.failure`，停下期间不画进度条）：危险色
  标题（`startup.failure.title`，如“无法读取主机簿”）、完整原因（换行，`startup.failure.reason`）与一句说明
  （`startup.failure.hint`）；面板下方是两个按钮（`startup.failure.actions`，T9 起不在面板里，而是在可滚动的文字区之外）：
  - **继续**（主按钮，`startup.continue`；启动页有焦点时 Enter 同效）：剩余阶段继续执行，之后的失败不再停下；进入首页后
    读不到的部分按原有降级显示（例如首页的 `state.card` 与原因）。
  - **打开配置文件夹**（`startup.open_folder`）：以 `file://` 地址（`folder_url`，百分号编码）交给系统文件管理器打开数据目录；
    启动页仍等待“继续”。
  停下的启动页不会自行超时，也不会闪退。布局（T9，`splash_body`）：标题行之下的区域把“品牌 + 停止面板”与按钮行一起
  垂直居中；内容放得下时按钮紧跟在说明下方，放不下时（115% 文字的 720×480）只有品牌与文字的部分（`startup.body`）收缩并
  纵向滚动，按钮行不收缩、始终在首屏可见（`the_stop_scrolls_to_its_buttons_in_the_smallest_window_at_the_largest_text`
  先断言两个按钮不滚动就在窗口内且可点，再断言滚动后仍可点）。
- **阶段耗时**：开发构建（`debug_assertions`）或设置了环境变量 `SHELLUNE_STARTUP_LOG` 的任意构建，在 stderr 逐个写出
  `shellune start: stage i/4 <阶段>: <毫秒>`，最后写出 `page shown after <毫秒>`（从构造视图算起）。单元测试不输出。
  本机实测（Windows，真实数据副本经 `APPDATA` 隔离、`SHELLUNE_STARTUP_LOG=1`，各三次，T9 于 2026-11-19 重测）：

  | 构建 | 阶段 1 设置与主题 | 阶段 2 主机簿与已知主机 | 阶段 3 转发分组与工作区 | 阶段 4 字体预热与首帧 | 四阶段合计 | 首页出现 |
  | --- | --- | --- | --- | --- | --- | --- |
  | 开发（`cargo build -p shellune`） | 1.1–1.4ms | 7.5–8.5ms | 5.3–6.9ms | 1.2–1.3ms | 约 16.5ms | 258–266ms |
  | 发布（`cargo build --release -p shellune`） | 0.7–0.9ms | 3.7–4.2ms | 4.2–4.7ms | 1.4–3.6ms | 约 10–13ms | 252–268ms |

  两种构建首页都在约 250–270ms 出现，由 250ms 品牌时刻决定，阶段本身不是瓶颈。未测进程启动到第一帧的时间。
- **推迟到 Ready 的工作**：任务中心的存储与中断恢复（`batch.recover`）、自动同步的首次拉取（`start_sync`）、
  更新清理与 20 秒后的检查（`start_updates`）、工作区布局恢复，都在采纳之后由 `begin_work` 一次启动；启动期间
  `layout_persist` 为关，不会把空布局写回 `workspace.json`。
- **标题行**：启动期间顶部是与正式标题行等高的空白拖动区（`startup.drag`），Windows 上右端是同一组窗口按钮；
  macOS 的红绿灯由系统画在同一行，Linux 用系统标题栏。窗口在读取期间、包括停在失败处时，都能拖动、最小化、最大化与关闭。
- **测试与截图**：`ShelluneView::new` / `with_workspace` / `with_store*` 仍在返回前同步读完所有存储（测试与截图用的
  “同步就绪”入口，首帧就是页面）；`launching_for_test` 走与启动相同的分阶段路径。测试覆盖阶段顺序与进度目标、阶段 1 与
  阶段 2 失败、继续与 Enter、打开配置文件夹、减少动效（不缩放、不位移、照常淡出）、系统动效回答的等待与超时、
  720 宽 115% 下的适配与失败滚动；旧 200% 图是 §7 的历史记录。截图场景 `startup-settings`、`startup-hosts`、`startup-workspace`、
  `startup-interface`（四个阶段）与 `startup-failed`（阶段 2 失败，固定原因）由 `capture_hold_startup(done, failure)`
  把窗口停在指定阶段并画出终态（截图固定减少动效，缩放、缓动与淡入淡出是静帧证明不了的），各有深浅两套，另有中文、
  720×480 变体（旧 200% 变体已取消）。
- **初始窗口背景**：GPUI 0.2.2 的 `WindowOptions` 只有不透明 / 透明 / 模糊三种窗口背景，没有颜色，因此
  `vendor/gpui` 加了一处补丁：公开函数 `gpui::set_initial_window_background(Rgba)`。Windows 注册窗口类时用这个颜色
  创建背景画刷（原为硬编码黑色）；之后打开窗口时颜色若已改变，在窗口显示前更新窗口类画刷。`open_main_window`
  在建窗口前按偏好里的主题与系统外观（`saved_theme_choice` + `App::window_appearance`）设为对应调色板的 `canvas`
  （`initial_floor`，测试 `a_new_window_opens_on_the_floor_of_its_saved_theme`）。
- **已知限制**：macOS 与 Linux 的补丁入口是空操作，窗口在首帧前仍是平台默认背景（macOS 不透明背景为黑色），浅色
  外观可能闪过一瞬；缓解仍是首帧只读偏好、以正确外观的 canvas 尽快覆盖窗口。Windows 上 GPUI 默认使用
  DirectComposition（`WS_EX_NOREDIRECTIONBITMAP`），窗口类画刷只在系统用 GDI 擦除背景时可见；首帧之前 DWM 实际
  显示什么，本机截图（读回 GPU 后备缓冲）无法证明，需要在真实启动中目视确认。GPUI 的动画按墙上时钟推进，测试只能断言
  首页位移的方向与上限（≤8px），不能断言某一帧的确切位置。

## 2. 视觉 tokens

2026-11-18 起是 **token v2**（全应用 UI/UX 重做，ADR-1）：极简工具风，中性灰阶加一个强调色，边框细、少用卡片、
阴影很淡，层级主要靠明度和边框区分。所有值只在 `src/app/theme.rs` 定义一次，视图只写令牌名，不写字面量
颜色、阴影预设、时长或尺寸；守卫测试（2.5）把这条规则变成断言。

### 2.1 颜色：两张独立的表

`theme::Token` 同时携带深色值与浅色值；两张表 `theme::dark` 与 `theme::light` 并排维护，浅色不由深色推导。
`theme::swatch(token)` 按当前外观解析；`theme::palette::*` 是视图唯一使用的令牌名。两个令牌可以共用同一个值
（例如 `CHROME` 与 `SURFACE` 都是面板层）。

唯一的有彩身份色是长春花蓝 accent，用于当前对象、主操作、焦点环与选中标记；常规控件不用渐变、不出现第二种
强调色，accent 不偏紫（蓝 > 绿 ≥ 红，蓝绿差 ≥ 0x40，由测试校验）。唯一的渐变 `brand_gradient`（accent → brand end）
只画在品牌徽记上。表面、边框与文字在两种主题下都保持中性（各通道差值 ≤ 14，由测试校验）。

**表面**（2026-11-20 护眼低眩光修订：深色采用柔和炭灰阶，浅色采用温和暖纸白，消除漆黑/纯白高眩光反差）：

| 令牌 | 深色 | 浅色 | 放什么 |
| --- | --- | --- | --- |
| `CANVAS` | `#18191d` | `#f4f2ed` | 窗口底：内容区、终端外围 |
| `SURFACE` | `#1e2025` | `#eceae4` | 面板层：侧栏、输入框、行、卡片 |
| `CHROME` | `#1e2025` | `#eceae4` | 标题栏、标签条、状态栏（与 `SURFACE` 同值，同属面板层） |
| `RAISED` | `#24262c` | `#faf9f5` | 浮层：菜单、弹出层、命令面板、tooltip、toast（`components::popover_surface`） |
| `OVERLAY` | `#282a30` | `#faf9f5` | 对话框（最前层，压在 scrim 上） |

深色下 canvas < 面板 < 浮层 < 对话框逐级变亮（测试校验）；浅色下面板比暖纸底色灰一档，浮在上面的一律为温和暖白
（测试校验浮层亮于 canvas 与面板）。

**交互填充是半透明遮罩**（ADR-1 的“白 5% / 黑 4%”本意）：`SURFACE_HOVER` / `SURFACE_SELECTED` / `SURFACE_PRESSED`
是 `Token::veil`，带各自的不透明度，`theme::swatch` 解析出带 alpha 的颜色，叠在元素所在的任何一层上——面板、菜单
（raised）、对话框（overlay）都会亮一档；选中行上的 tag（同为 selected 底）再叠一层，不会消失。需要盖住下层内容的
地方（终端输入法组字框）用 `theme::swatch_over(veil, layer)` 取叠好的不透明色。对比度测试一律先用 `Token::over`
把遮罩叠到具体的层上再量（`contrast::both` 遇到遮罩直接报错）。发丝线仍是叠在面板上的不透明值。
测试 `the_interaction_fills_and_hairlines_are_the_documented_blends` 校验比例，`every_interaction_fill_stands_off_every_layer_it_is_drawn_on`
校验每种遮罩在五个层上、以及选中行上的 tag 与其底都至少差 1.08:1：

| 令牌 | 深色 | 浅色 |
| --- | --- | --- |
| `SURFACE_HOVER` | 白 5%（叠面板 ≈ `#292b30`，叠对话框 ≈ `#33353a`） | 黑 4% |
| `SURFACE_SELECTED` | 白 8% | 黑 6% |
| `SURFACE_PRESSED` | 白 10% | 黑 8% |
| `BORDER` | 白 7% 叠面板 → `#2e3034` | 黑 8% 叠面板 → `#d9d7d2` |
| `BORDER_STRONG` | 白 12% 叠面板 → `#393b3f` | 黑 14% 叠面板 → `#cbc9c4` |

**文字、强调色与状态色**：

| 令牌 | 深色 | 浅色 | 用途 |
| --- | --- | --- | --- |
| `TEXT_PRIMARY` / `TEXT_SECONDARY` / `TEXT_MUTED` | `#dce0e7` / `#b8bcc6` / `#a7aab4` | `#24262b` / `#4f5158` / `#595b63` | 标题 / 正文 / 提示与计数 |
| `ACCENT` / `ACCENT_HOVER` / `ACCENT_PRESSED` | `#9fa3f5` / `#acb0f7` / `#9296f1` | `#464cb8` / `#3d43ab` / `#353b9e` | 主操作：静止 / 悬停 / 按下 |
| `ON_ACCENT` | `#18191d` | `#faf9f5` | 实心 accent 上的文字 |
| `ACCENT_TINT` / `ACCENT_ON_TINT` | `#303242`（accent 14% 叠面板）/ `#cad0fa` | `#e8e8ef`（accent 10% 叠暖白）/ `#393fa8` | 徽标、空状态图标块、当前项底色 |
| `POSITIVE` / `WARNING` / `DANGER` / `INFO` | `#73c99b` / `#dcb56b` / `#ef9494` / `#84b6ee` | `#1b663f` / `#7a4f04` / `#a82f37` / `#265ca1` | 低饱和状态色（同时是其 tint 上的文字色） |
| `*_TINT`（positive / warning / danger / info） | `#2a3836` / `#39352f` / `#3b3035` / `#2c3541`（各 14% 叠面板） | `#e4eae3` / `#ede8dd` / `#f2e5e2` / `#e5e9ed`（各 10% 叠暖白） | 状态 chip、横幅、toast 图标底 |
| `PLANNED` | `#a9acb4` | `#555661` | 已关闭 / 本版本不提供 |
| `FOCUS_RING` | `#acb0f7` | `#3d43ab` | 键盘焦点环（2px 边） |
| `DISABLED_SURFACE` / `DISABLED_TEXT` | `#212329` / `#878a94` | `#e8e6e0` / `#74757c` | 不可用控件（保持 3:1） |
| `LOADING_TRACK` / `LOADING_FILL` | `#303239` / `#9fa3f5` | `#dedcd6` / `#464cb8` | 进度条的槽与已完成部分 |
| `SEGMENT_TRACK` / `SEGMENT_THUMB` | `#141519` / `#2c2f36` | `#e2e0da` / `#faf9f5` | 分段控件凹槽 / 选中滑块 |
| `TRACK_OFF` / `TRACK_OFF_HOVER` | `#565963` / `#626570` | `#82817c` / `#74736e` | 开关关闭时的轨道 |
| `TRACK_ON` / `TRACK_ON_HOVER` | `#7479e0` / `#7c81e6` | `#464cb8` / `#3d43ab` | 开关打开时的轨道（accent 色相，深色下压暗以托住浅色圆钮） |
| `KNOB` | `#f0f1f4` | `#faf9f5` | 开关圆钮 |
| `DANGER_SOLID` / `ON_DANGER` | `#c42b1c` / `#f7f7f8` | `#b82e24` / `#faf9f5` | 实心危险填充（Windows 关闭按钮悬停） |
| `SCRIM` | `#000000` | `#202124` | 对话框遮罩，按 `SCRIM_ALPHA`（0.62）叠加 |
| `SHADOW` | `#000000` | `#000000` | 层级阴影的墨色 |
| `SELECTION` | `#434663`（accent 32% 叠深色终端底 `#181a1f`） | `#cfcfe3`（accent 22% 叠浅色终端底 `#f6f4ef`） | 终端选区背景，按**终端**配色解析 |
| `AVATARS` | 8 组 | 8 组 | 主机头像（靛、钢、青、鼠尾草、苔、沙、陶、石楠，各 18%/12% 叠底），低饱和，不与状态色同源 |
| `TAB_ACCENTS` | 6 色 | 6 色 | 标签辨识色（不与状态色同源） |

**与早期初版的偏差（护眼低眩光与对比度守卫）**：2026-11-20 起深色表面整体由近纯黑（`#0f0f11`–`#202024`）抬升至柔和炭灰
（`#18191d`–`#282a30`），浅色表面由近纯白（`#fafafa`/`#ffffff`）调为温和暖纸白（`#f4f2ed`/`#eceae4`/`#faf9f5`），
正文与状态色同步降眩光并在全部 20 种表面（5 层 + 3 种交互遮罩 × 5 层）上严格保持 ≥ 4.5:1 对比度；深色 `ON_ACCENT`
取 `CANVAS` 炭灰 `#18191d`，浅色取暖白 `#faf9f5`。中性色保持通道差 ≤ 14，强调色保持长春花蓝色相（`b > g >= r` 且 `b - g >= 0x40`）。

**对比度（测试断言）**：正文三色、accent、四个状态色与 planned 在五个层（canvas、chrome、surface、raised、
overlay）以及 hover/selected/pressed 叠在每个层上的 15 种底上深浅两套都 ≥ 4.5:1；焦点环 ≥ 3:1；每个状态色在自己的 tint 上、
`ON_ACCENT` 在三种 accent 上、`ON_DANGER` 在 `DANGER_SOLID` 上、头像字在头像底上 ≥ 4.5:1；禁用文字与进度条
取图形下限 3:1；开关圆钮在四种轨道上、打开的轨道在各表面上深浅两套都 ≥ 3:1。

**终端深浅色板（`crates/terminal/src/palette.rs`、`theme::light::TERMINAL`）**：
深色终端采用柔和炭灰底 `#181a1f`、柔和浅灰白前景 `#d8dce4` 与柔和长春花蓝光标 `#8ea0e6`，16 个 ANSI 槽位降饱和去荧光，
满足 `ANSI[0]` 对比度 `< 4.5`、`ANSI[1..16]` ≥ 4.5:1、亮色严格亮于常规色、dim 严格暗于常规色。
浅色终端采用温和暖纸白底 `#f6f4ef`、柔和石墨灰前景 `#282c34` 与深长春花蓝光标 `#4858b8`，`theme::light::TERMINAL`
完整映射全部 19 个默认与 ANSI 槽位并由测试守卫全部达到 ≥ 4.5:1 对比度。程序通过 OSC 或真彩色主动输出的颜色不被改写；
终端配色可单独选择（`theme::TerminalScheme`）。绘制时有最低对比度保护：低于 3:1（dim 2:1）时把文字推向黑或白。

### 2.2 层级（Elevation）

`theme::Elevation { Flat, Raised, Overlay }`。视图写 `.elevation(Elevation::Overlay)`（`theme::Elevated`），
不调用 GPUI 的 `shadow_*` 预设。每一级都保留 1px 边框，阴影只增加存在感；深色下阴影很淡。

| 级 | 用于 | 浅色（y / blur / spread / α） | 深色 |
| --- | --- | --- | --- |
| Flat | 行、chrome、行内控件、Secondary/Ghost 按钮 | 只有边框 | 只有边框 |
| Raised | Primary 按钮、分段滑块、开关圆钮、`card` / `state_card` | 1/2/0/.06 | 1/2/0/.24 |
| Overlay | 菜单、弹出层、tooltip、toast、状态卡、对话框、命令面板 | 1/2/0/.04 + 8/24/0/.12 | 1/2/0/.20 + 8/24/0/.36 |

旧的 Low / Mid / High 分别并入 Raised / Overlay / Overlay。测试校验 Flat 无阴影、Raised 的外延是 0 1 2、Overlay
是 0 8 24，以及深色阴影 α ≤ 0.4。

### 2.3 动效

gpui 没有 hover 过渡、没有元素缩放，也不报告系统的"减少动态效果"设置（应用自己读取，见第 5 条），因此规则是：

1. **只有进入动画，退出只有对话框遮罩淡出与启动页淡出**（后者见第 1.5 节）。hover、按下、选中都即时切换；
   除进行中指示（连接中的状态点、不确定进度条、骨架屏）外没有循环动画。对话框关闭时卡片立即消失，只有不带状态的遮罩用
   BASE 淡出，减少动效时也立即消失。
2. 时长令牌：`Motion::FAST` 120ms（小状态变化、toast/tooltip 淡入）、`Motion::BASE` 200ms（菜单、卡片、等待徽记）、
   `Motion::SLOW` 300ms（对话框、启动后首页淡入）；启动页品牌时刻 `BRAND_ENTRANCE` 250ms；进行中的循环脉动一拍 `Motion::PULSE` 900ms，不确定进度条一趟
   `components::PROGRESS_SWEEP` 1200ms。缓动统一 `theme::ease_out()`：**ease-out-cubic**（`1 − (1 − t)³`，测试校验）。
3. `theme::enter(element, id, Enter::{Fade, Rise, Settle})`：Fade 原地淡入（FAST）；Rise 淡入并上移 6px（BASE）；
   Settle 从上方 8px 落定（SLOW，替代做不到的轻微缩放）。同一 id 重绘不会重播。位移不改变元素的定位方式。
4. 减少动效时只保留淡入淡出：`enter` 直接画最终状态，循环脉动静止，不确定进度条画成居中的静止段，状态词、颜色与
   操作都不变。启动是唯一在减少动效时仍播放淡入淡出的地方：`theme::arrive(element, id, duration, travel)` 总是淡入，
   位移由调用方按动效选择（减少动效时为 0），见第 1.5 节。
5. **减少动效 = 用户偏好 或 系统设置**：窗口每帧用 `Motion::with_system` 合并，任一方要求减少即为减少。系统设置由
   `theme::system_reduces_motion()` 读取（Windows `SPI_GETCLIENTAREAANIMATION`、macOS
   `accessibilityDisplayShouldReduceMotion`、GNOME `enable-animations` / KDE `AnimationDurationFactor=0`）；读不到时视为
   未要求。截图固定为减少动效。

### 2.4 形状、间距与字

**圆角** `theme::radius`：4 / 6 / 8 / 12 四档，物体越大圆角越大。

| 档 | 令牌 | 用于 |
| --- | --- | --- |
| 4 | `SMALL` | 标签（tag）、chip、徽标、键帽、勾选框、分段、菜单项（与菜单卡 4px 内边距同心） |
| 6 | `MEDIUM` | 按钮、输入框、搜索框、列表行、侧栏项与设置导航项、命令面板结果行 |
| 8 | `LARGE` | 卡片、浮层（菜单、命令面板、tooltip、toast、浮动控件条，ADR-7）、终端框、标签页顶部 |
| 12 | `XLARGE` | 对话框、状态卡、启动标志 |

旧的 10 / 14 / 20 档已删除（`XXLARGE` 删除，用到它的地方改为 12）。1px 与 0 只用于细状态条与拼接边。

**间距** `theme::spacing`：2 `HALF`、4 `HAIRLINE`、6 `NARROW`、8 `TIGHT`、12 `SNUG`、16 `BASE`、20 `WIDE`、24 `LOOSE`、
32 `SPACIOUS`（4px 栅格加两个半档）。

**字阶** `theme::type_scale`：12 `CAPTION`（说明、元信息、状态栏）、13 `BODY`（正文与所有控件文字，`Metrics::label_size`
同为 13）、14 `EMPHASIS`（强调正文：横幅与卡片标题行）、16 `TITLE`（小标题：面板与对话框标题）、20 `HEADING`（页标题）、
28 `HERO`（品牌）。旧的 label 12 / display 22 已删除。**字重** `theme::weight`：400、500、600，不用 bold。

**等宽端点**：地址、端口、路径、远端命令用终端等宽字体（`components::endpoint`），周围标签仍用界面字体。

**尺寸**：控件 32（紧凑 28、大号 36，`Metrics::{compact_control_height, control_height, large_control_height}`），
行 52，标签 30，头部 46，指针目标 ≥ 24。所有长度经 `Metrics` 按文字大小（90% / 100% / 115%）缩放；终端外框与
tooltip 这类在窗口渲染之外绘制的部件用 `theme::metrics()` 取得同一缩放。

**边框**：1px 发丝线；焦点与校验失败 2px；按下时底边加粗到 2px。卡片用弱边，浮层用强边 + Overlay。

图标是 `icons.rs` 内嵌的 24 格线条 SVG（1.6 描边），按元素文字颜色着色：控件 14px、行与标题 16px、
状态徽记 20–22px；通知与状态语义配备专属图标（成功 `Check`、警告 `AlertTriangle`、危险/错误 `AlertCircle`、信息 `Info`）。

### 2.5 守卫测试与裸尺寸基线

`theme.rs` 的 `guard` 测试读取 `src/app/**/*.rs` 的非测试代码（跳过 `tests.rs` 与 `#[cfg(test)]` 条目，忽略字符串与注释）：

- **禁止颜色字面量**：`rgb(<数字>`、`rgba(<数字>`、`hsla(<数字>` 与六位以上的十六进制值只允许出现在 `theme.rs`
  （两张调色板、头像与标签表、终端 ANSI 表）和 `capture.rs`（PNG 的 CRC 表，不是颜色）。
- **裸尺寸只降不升**：统计 `scale(<数字>)` 与 `px(<数字>)` 的个数，超过基线即失败；基线随迁移逐步下调。

`px(0)` 不计入：零不是需要命名的尺寸。T3 第一次提交把约 250 处 `min_w(px(0.0))` / `min_h(px(0.0))` 机械改成
`min_w_0()` / `min_h_0()`，若把零算在内，`px` 从 407 掉到 154 会直接满足“一半以下”，却没有任何界面迁移到度量上；
因此守卫只数非零的 `px(<数字>)`，迁移前的数字也按同一口径重算（同一套统计逻辑跑在 `d59a63f` 的 `src/app` 上，
结果与测试在当前代码上打印的计数一致）。

| 时点 | `scale(<数字>)` | `px(<非零数字>)` | 说明 |
| --- | --- | --- | --- |
| 迁移前（`d59a63f`，T3 记录） | 280 | 91（另有 316 处 `px(0)`，不计） | ADR-9 的“一半以下”以这一行为准：最终需 `scale` ≤ 140、`px` ≤ 45 |
| T3 之后 | 267 | 82 | 新组件只用 spacing 令牌；状态标记与选中竖条的 1 / 1.5 / 2px 收为 `MARK_CORNER` / `MARK_STROKE` / `MARK_BAR_WIDTH` |
| T4 之后 | 220 | 82 | 首页改为侧栏 + 单列表：卡片带、快速连接栏、分组列与分组头删除；新代码的尺寸都是命名常量（`SIDEBAR_WIDTH` 等）或 spacing 令牌 |
| T6 之后 | 203 | 75 | 标签改为胶囊（标签底色、分隔线与远程画面取色删除）；底栏终端工具收进菜单（折叠测量与 `tools_width` 删除）；状态卡改为横幅，横幅只用 spacing 令牌与命名常量（`DIAGNOSTIC_MAX_HEIGHT`、`BANNER_WORDS_WIDTH`）；标签条溢出容差 `STRIP_ROUNDING_SLACK` |
| T7 之后 | 163 | 66 | 文件栏头、面包屑、文件行、问题、传输条与进度条、远程桌面状态卡与证书卡、隧道表单改用组件与 spacing / 度量令牌；余下的长度是命名常量（`FILES_SIZE_WIDTH`、`FILES_SELECTION_WORDS_MIN`、`NOTICE_WIDTH`、`ZOOM_LABEL_WIDTH`、`FORWARD_PORT_WIDTH`、`FORWARD_ROW_WORDS_MIN`、`LIST_ROW_MIN_HEIGHT`、`TOOLBAR_MIN_HEIGHT`、`HAIRLINE_WIDTH`）；状态卡徽记、浮动条、竖线与选中竖条的裸数换成等值的度量（外观不变） |
| T8 之后 | 99 | 43 | 设置、对话框、菜单、命令面板、toast、tooltip 与批量任务迁移：设置导航改用 `nav_item`，选中竖条改用 `accent_mark`，设置页、批量任务、命令面板、标签菜单 / 标签列表、主机表单与快捷命令编辑器余下的长度改为命名常量（`SECTIONS_MAX_WIDTH`、`PAGE_MAX_WIDTH`、`ROW_LABEL_MIN_WIDTH`、`KEYS_NAME_MIN_WIDTH`、`NUMBER_FIELD_WIDTH`、`LIST_MAX_HEIGHT`、`RUN_OUTPUT_HEIGHT`、`PALETTE_WIDTH`、`PALETTE_LIST_MAX_HEIGHT`、`MENU_MIN_WIDTH`、`MENU_MIN_HEIGHT`、`TAB_MENU_MIN_WIDTH`、`TOAST_WIDTH`、`ICON_TILE`、`KBD_HEIGHT`、`FORM_FIELD_MIN_WIDTH` 等）或度量（菜单项、命令面板脚注与跳板选择器行改为 `compact_control_height`，命令面板结果行为 `large_control_height`）；对话框标题、状态卡标题回到字阶（`title_size`），宽度计算里的 2px 边框写作 `HAIRLINE_WIDTH × 2`。已低于 ADR-9 的 `scale` 目标（≤ 140），`px` 距目标（≤ 45）已达成 |
| **T9 之后（最终基线）** | **13** | **2** | 收尾清理（取值不变，外观不变）：组件库余下的长度全部命名（`STATUS_DOT_SIZE`、`ATTENTION_MARK_SIZE`、`CHIP_HEIGHT`、`KBD_TEXT_SIZE`、`BADGE_SIZE`、`KIND_BADGE_HEIGHT`、`BANNER_EDGE_WIDTH`、`FIELD_ROW_LABEL_WIDTH`、`STATE_EMBLEM_SIZE`、`STATE_EMBLEM_ICON_SIZE`、`STATE_CARD_WORDS_MAX_WIDTH`、`LOADING_TRACK_WIDTH`、`LOADING_FILL_WIDTH`、`LOADING_BAR_THICKNESS`、`SWITCH_WIDTH`、`SWITCH_HEIGHT`、`SWITCH_INSET`、`CHECKBOX_SIZE`、`CHECKBOX_TICK_SIZE`、`EMPTY_EMBLEM_SIZE`、`EMPTY_EMBLEM_ICON_SIZE`、`EMPTY_WORDS_MAX_WIDTH`、`MODAL_ICON_TILE`、`SKELETON_LINE_HEIGHT`、`SWITCH_WORD_MIN_WIDTH`）；终端与输入框（`SESSION_SLOT_MIN_WIDTH`、`SEARCH_PANEL_MAX_HEIGHT(_CAP)`、`SEARCH_QUERY_MIN_WIDTH`、`SEARCH_FOCUS_HALO`、`SCROLL_THUMB_INSET/WIDTH`、`LAYOUT_SLACK`、`CARET_INSET`、`COMPOSITION_UNDERLINE`、`FOCUS_HALO`、`FIELD_MENU_MIN_WIDTH`）；标签条、顶栏、分屏与各页的零散尺寸（`STRIP_FADE_WIDTH`、`TAB_ICON_SIZE`、`TAB_TITLE_LINE_HEIGHT`、`TAB_ACCENT_MARK_WIDTH/HEIGHT`、`HOST_AVATAR_SIZE`、`SHELL_PICKER_WIDTH`、`BRAND_MARK_SIZE`、`BAR_ICON_SIZE`、`DIVIDER_BAND`、`PANE_HEAD_HEIGHT`、`CAPTION_GLYPH_SIZE`、`DOWNLOAD_BAR_HEIGHT` 等）；与已有令牌同值的改写成令牌（`scale(2.0)` → `spacing::HALF`、`scale(16.0)` 图标槽 → `icon_size()`、`scale(28.0)` → `compact_control_height()`、`scale(24.0)` → `HIT_TARGET` / `ICON_TILE`、`px(16.0)` → `spacing::BASE`），窗格活动竖条改用 `accent_mark`，独立的发丝线与边框宽写作 `HAIRLINE_WIDTH`，`x / px(1.0)` 取数改为 `f32::from(x)`；截图工具的窗口位置与右键坐标也命名（`OFF_DESKTOP`、`TAB_MENU_AT`、`HOST_MENU_AT`、`FILES_MENU_AT`）。余下的 13 个 `scale(<数字>)` 是 `theme.rs` 里 `Metrics` 度量本身的定义（`rail_width`、`header_height`、`list_row_height`、`icon_size`、`small_icon_size`、`control_height`、`compact_control_height`、`large_control_height`、`tab_height`、`tab_min_width`、`tab_compact_min_width`、`name_min_width`、`tab_max_width`），余下的 2 个 `px(<数字>)` 是已命名的 `STRIP_ROUNDING_SLACK`（`Pixels` 常量）与标签条滚动步长的 1px 下限。相对迁移前（280 / 91）分别降到 4.6% 与 2.2%，远低于 ADR-9 的一半（≤ 140 / ≤ 45） |

### 终端选择（R4，ADR-5，`src/app/terminal.rs` + `crates/terminal`）

选区的边界在格与格之间，而不在格子上；规则与常见终端一致，由 alacritty 的 `Selection` 按半格计算。

| 规则 | 行为 |
| --- | --- |
| 半格 | 指针在格子左半部为 `Side::Left`，右半部为 `Side::Right`，正好在中线算 Left。`cell_at` 返回 `((行, 列), Side)`；x < 0 取第 0 列的 Left，超出右边界取末列的 Right，y 越界夹到首行或末行。按下与拖动都传实际的 Side：从字符左半部起拖包含该字符，从右半部起拖不包含，与拖动方向无关 |
| 拖动阈值 | 按下只记录起点（同时清除旧高亮）；移动超过 `SELECTION_DRAG_THRESHOLD`（3px，欧氏距离）或进入另一格才开始选择。松开时还没开始就按单击处理：清除选择，不复制 |
| 双击 / 三击 | 双击选词、三击选行，按下即生效，松开即复制（行选择照旧带结尾换行） |
| Shift+单击 | 已有选择时把终点移到点击处，继续拖动继续扩展；没有选择时从光标格的 Left 选到点击处。程序接管鼠标（鼠标上报模式）时，Shift+拖动从按下点开始做本地选择（程序自己管光标），整个过程不向程序上报 |
| Alt / Option+拖动 | 块选（`SelectionType::Block`）：每行同一组列 |
| 窗口级捕获 | viewport 的 canvas 每帧注册 `window.on_mouse_event`（MouseMove / MouseUp，Bubble 阶段），只有本窗格持有拖动时才生效：指针离开终端或窗口后选择仍跟随，松开也能收到；收到 `pressed_button ≠ Left` 的移动按在该处松开处理。与 ADR-5 措辞（“开始后注册、松开时注销”）的差别：按下后第一帧重绘之前可能已有移动事件，按需注册会漏掉，每帧注册、无拖动时不做事在效果上等价 |
| 边缘自动滚动 | 指针越过 viewport 上下边缘时，每 `SELECTION_AUTOSCROLL_INTERVAL`（50ms）滚动 ceil(越界距离 / 行高) 行，限制在 1–`SELECTION_AUTOSCROLL_MAX_LINES`（5）行，终点同时更新到边缘行；回到区域内或松开即停。只在主屏有历史时滚动（备用屏没有历史） |
| 选区颜色 | `SELECTION` 令牌（深浅各一），不再是视图里的字面量 |

未覆盖：程序接管鼠标后由程序处理的普通拖动（非 Shift）仍只靠悬停分发，没有窗口级捕获（不在 ADR-5 范围）。

测试（`src/app/tests.rs`）：`a_drag_from_the_left_half_of_a_character_includes_it`（截图里 `test-c9685ba2` 的 `t` 用例）、
`the_half_of_a_cell_decides_whether_it_is_selected`、`a_press_becomes_a_selection_past_the_drag_threshold`、
`shift_click_extends_the_selection`、`alt_drag_selects_a_block`、`a_drag_that_leaves_the_grid_keeps_selecting`、
`a_drag_past_the_edge_scrolls_the_history`、`shift_drag_selects_locally_while_a_program_has_the_mouse`、
`double_and_triple_clicks_select_a_word_and_a_line`、`a_mouse_selection_is_copied_when_it_is_released`。截图场景（T9 新增）：
`terminal-selection`（从 “42 passed” 的 “4” 左半部到 “部署完成” 末字右半部，跨两行与宽字符；深浅、中英、1120×720；旧 720×480 @200% 变体已取消）与 `terminal-selection-block`（“grid” 到 “paste” 的块选；深浅、中文；旧 200% 变体已取消）。选区由截图工具经
`TerminalPane::capture_select` 按文字定位后铺上，网格改宽时重新铺一次；截图证明不了阈值、Shift、窗口级捕获与自动滚动，
这些由上面的 GPUI 测试覆盖。

### 终端内搜索（`crates/terminal` + `src/app/terminal.rs`）

搜索条在网格下沿打开，读的是当前窗格真实的滚动历史，不另存一份日志：查询在 `TerminalCore` 里按
"字面 / 正则 + 是否区分大小写"（`SearchOptions`）编译一次，历史每行里的每一处命中各自算一处
（`HistoryMatch` 记录网格行、字节列、长度与该行文本），命令块列表用同一组开关匹配命令与输出，
因此不会出现两处搜索各读一套选项。查询为空时不产生命中、也不显示计数；命中为空与表达式无法编译是
两种结果：前者写"没有匹配"，后者写"正则表达式无效"加引擎给出的原因，终端内容与会话状态都不变。

搜索条从左到右：查询文本（等宽字体，空查询时显示占位文案）、键盘归属提示、位置（`3 / 12`）、上一个匹配、
下一个匹配、`[x] 区分大小写`、`[x] 正则表达式`、关闭搜索。两个开关的方框写出状态（未打开为 `[ ]`），填充色只重复它，
状态不单靠颜色表达。搜索条的按钮高度取 `Metrics::hit_target()`，90% 文字下也不低于 24×24。

条按窗格宽度排布，不按窗口：一行放不下就换行（`flex_wrap`），按钮与开关保持整块不拆字，查询框至少留 80 px，
键位提示这类整句文字可以先被压窄、再在自己的行里换行——它比一个窗格还宽（英文提示约 509 px），
在最小窗口横向分栏时（每格 354 px，条内 336 px）若不允许收缩就会被画到窗格右缘之外，关闭键的说明也跟着看不见。
计数、空结果和非法正则原因是同一类整句，同一条规则：可以先被压窄，再在自己的行里换行。
`a{2,1}` 的原因（重复次数的起点大于终点）比分栏后的窗格宽；钉成 `flex_none` 时尾部画出窗格右缘，被窗格的 `overflow_hidden` 裁掉。
条所在的命令块面板最高 180 px（按文字大小上限 260 px），内容更高时在面板内纵向滚动，关闭与两个开关因此始终可达。
720 宽 + 115% 文字（中英文）与分栏窗格下，条的全部部件都在窗口与所在面板/窗格内，由
`src/app/tests.rs` 的 `the_find_bar_stays_inside_the_window_at_the_large_text`、
`the_find_bar_narrows_with_the_pane_instead_of_leaving_it`、`a_find_bar_in_a_split_pane_stays_inside_its_pane`
与 `a_broken_expression_reason_wraps_inside_the_narrow_pane`
按测量到的 bounds 锁定。

键盘：搜索条持有键盘时 Enter 下一处、Shift+Enter 上一处（两端循环）、Backspace 编辑查询、Alt+C 切换区分大小写、
Alt+R 切换正则表达式、Esc 关闭搜索并把键盘交回终端，因此只用键盘就能打开搜索、调两个开关、逐处定位再关闭（R1、R4）。
Alt+C 与 Alt+R 是搜索条的编辑键，不进动作目录：它们不是应用动作，只是条内的按键，并且只在搜索条持有键盘时被截获，
条关闭后这两个组合键原样交给 Shell（readline 的 capitalize-word 与 revert-line 仍然可用）。条上同时写出这几个键；
搜索条持有键盘时粘贴的文本也只进查询（换行变空格），不会作为命令写进 Shell。终端本身仍独占 Ctrl+C、Ctrl+L 与其余按键。
打开与关闭走 T1 的动作目录 `terminal.search`（Windows/Linux `Ctrl+Shift+F`，macOS `⌘F`），可改键或解除绑定。

键盘归属写在条上，不靠猜：条持有键盘时查询区画 accent 边框与柔和焦点环，提示行列出编辑键；点击网格后搜索条仍打开、
计数继续显示，但查询区改为中性边框，提示行改为"键盘已回到终端"，此时 Enter 交给 Shell 执行命令而不是跳到下一处（R4）。

打开搜索记下当时的滚动位置，定位命中时滚动到对应行；关闭时恢复打开前的滚动位置并把焦点还给终端，
不把读者留在最后一条命中上。窗口尺寸或分栏改变会让主屏重新折行，命中数与可定位行都随之变化：搜索条打开期间每次
resize 都按同一条 recount 路径重新计数，视口停在读者原处，不会拿旧行号去定位（R1）。
备用屏幕上的全屏程序内容不是可搜索的历史：那里不打开搜索条，也不截获输入。

### 标签身份与全部标签列表（R2）

一个标签有两样属于自己的东西：可以改的名字和可以选的辨识色。两者都只影响标签本身，不改目标、类型、状态或窗格。

- 名字：设置后顶栏标签、命令面板和标签列表都显示这个名字；标签的自动标题仍被保存着，"恢复自动标题"只清掉覆盖值。
  自动标题只在没有自定义名字时按同名序号区分（`pwsh 1`、`pwsh 2`）；自定义名字按原样显示，同名时靠目标、类型和状态区分，
  这三样在悬停提示、标签本身和标签列表里都写出来；头部第二行不再重述名字。
  名字上限 64 个字符（`MAX_TAB_NAME_CHARS`）：超出的部分不保存，但**不会静默**——输入超过上限时弹窗立即显示一行提示
  （"标签名称最多 64 个字符；保存时只保留前 64 个，其余不保存"），保存后弹窗留在已保存的那个名字上，并换成
  "已保存前 64 个字符；修改名称后再次保存可更新"。字段里显示的与标签实际保存的是同一个名字，再次保存（或 Esc）关闭弹窗。
- 辨识色：六个固定色（板岩蓝、石南、钢青、鼠尾草、沙、灰粉，`theme::palette::TAB_ACCENTS`）之一，画在状态点左侧的
  3px 竖条上；没有设置时不画。色板与状态色（green/amber/red）分属不同 token，浅色与深色主题下的对比度由测试校验，
  色名同时以文字出现在标签列表里，因此颜色从不单独承担识别。
  **色块是可点击控件，和旁边的按钮一样有指针反馈**：静止时无填充（选中色带 `text primary` 描边环）；悬停时填充
  `surface hover`、描边 `border strong`；按下时填充再上一级 `surface selected`、描边 `accent`。选中色在三种状态下都保留
  自己的环，因此"当前是哪个颜色"不会因为指针停在别的色块上而消失。填充与环由 `theme::accent_swatch_face(chosen, state)`
  一处给出（`SwatchState::{Resting, Hovered, Pressed}`），视图只读它，测试也读它。这三种指针状态都不表达状态语义：
  环表示选择，填充表示指针，二者都与会话状态无关。
- 入口：标签右键菜单里有"重命名…"和一行色块（点色块立即生效，再点"不设颜色"清除）。"重命名…"打开一个小弹窗：
  名称输入框（Enter 保存，Esc 关闭）、同一行色块、"恢复自动标题"、"取消"、"保存名称"；空名字被拒绝并说明原因，
  已输入内容保留。两处色块行（标签菜单 `tab.menu`、命名弹窗 `tab.rename`）画同一组色块、给同一种反馈。
  命令面板为当前标签提供四件事："重命名标签…"、"标签辨识色"（以当前 → 下一个的顺序显示，可一直循环）、
  "恢复标签自动标题"、"不设标签辨识色"。后两条是纯键盘也能"撤销"的路径：循环色永远回不到"不设颜色"，
  清除名字与清除颜色此前只有弹窗里的按钮，现在两端都在面板里，检索词与中英文案一致。
- 全部标签：标签多到顶栏放不下时，标签行横向滚动，"全部标签"按钮画在标签行之外（新建、搜索、设置按钮同样在行外），
  任何标签数量下都留在窗口里；所有标签都可见（且非紧凑）时不画这个按钮（规则见 1.0）。它打开一个共享的居中弹窗（与设置、快捷命令、粘贴确认同一套 `modal` 外观），按顶栏顺序
  列出每个标签：色条与色名、名字、类型徽标、目标、状态文字与状态点，当前标签带"当前"chip；点击一行切换过去，
  行末的铅笔为该标签改名、叉号关闭该标签。类型徽标写的是**会话自己的协议**（本机 / SSH 远程 / 远程桌面 / 串口，
  `kind_badge_parts` 一处给出，命令面板用同一份映射），所以 RDP、VNC 与串口标签不会被画成本机标签。
  叉号走的是顶栏标签自己的关闭入口（`request_close_tab_anchor`），因此正在传输或转发的会话仍先问、只取消自己那条任务，
  也不会关错标签；关闭后弹窗随之收起，键盘回到原上下文。弹窗打开时把键盘接到自己身上：↑↓ 移动高亮（打开时停在当前标签上），
  Enter 切换过去并关闭，Esc 关闭且不切换，指针悬停与上下键移动的是同一个高亮，因此点击会选中的行就是 Enter 会打开的行。
  每行右侧的键帽是**当前生效的**"跳到第 N 个标签"绑定（`tab_jump_action` + `Keymap::label`），改键或解除绑定后随之变化；
  底部提示行同样给出 Select / Open / Close 三组真实按键。弹窗是模态的：它打开时键盘不在终端里，按键不会写进 Shell 程序。
- 名字、颜色和顺序随工作区保存（`workspace.json` 的可选字段），重启后与布局一起恢复为待启动标签：不自动连接、
  不启动 Shell、不执行命令；没有这两个字段的旧文件照常读取。

### 后台提醒（R7）

不在当前标签的终端发生了两件事之一时，那个标签自己带上一个词，别处什么都不变：

- 来源只有两个：程序响铃（真实的 BEL 字节），以及 Shell 通过 OSC 133/633 结束标记报告"命令结束"
  （`crates/terminal` 的 `TerminalEffects::{bell, commands_finished}` → 窗格的 `TerminalAttention`）。
  没有结束标记的 Shell 不产生完成提醒：结束只来自 Shell 自己说的话，进程退出、缺少退出码的提示符、
  被整合失败关掉的块都不算。未知退出码也照实计为"结束"，不写成成功。
- 标记是词，不是色块：顶栏标签里写"响铃"或"结束"（`caption` 字号、主色 `accent`），标签列表里各给一个 chip，
  悬停提示写出这枚标记的含义与清除方式。颜色只重复"有新消息"，两种标记靠文字区分，浅色与深色主题一致。
- 词不压缩，标签自己让路：标签的最小宽度把两枚标记的字宽算进去（`attention_mark_room`，每枚按三个 `caption`
  字高估，覆盖英文五个字母与中文两个全角字），所以挤不下时先由顶栏裁掉整块标签，而不是让标记和它后面的关闭按钮
  滑到相邻标签上。标签内部可变的正文（标题、状态词、标记）放在自带裁切的行里，关闭按钮始终留在标签自己的边界内，
  点到的就是它自己的标签。拥挤的顶栏（多标签）与 720 宽 + 115% 文字（顶栏已无标签空间，标记改由标签列表读出）
  分别由 `a_marked_tab_keeps_its_marks_and_close_button_inside_itself` 与
  `the_tab_list_keeps_the_marks_readable_in_the_smallest_window_at_large_text`（中英文各量一次）按实测 bounds 锁定。
- 什么算"已查看"：是当前标签，不是当前聚焦的那个窗格。分屏标签把它的窗格同时画在屏幕上，所以同一标签里
  相邻窗格的响铃或命令结束同样属于"正在看"，既不标记、也会即时清掉该标签已带的标记；焦点在一个标签内移动
  （`focus_leaf` / `present_active_pane`，含点击窗格、方向键换窗格）同样清除该标签的未读（`SessionLayout::sessions_on_screen`）。
  缩放一个窗格只是把它单独画满，其它窗格仍属于同一个标签、同一次查看，所以它们同样不产生标记。
  R7 的标记属于"非当前标签"：把标记打在当前标签上既没有别的标签可跳转，也没有"下一次查看"来清除它。
- 不抢焦点：不切换标签、不动键盘、不弹 toast、不遮挡当前输入；标记落在标签自己的命中区里，点它就是切到那个标签
  与窗格，也就是查看，查看即清除该标签的未读标记（`Workspace::note_attention` / `clear_attention`）。
  未读状态只在本次运行内保存：它是"刚刚发生了什么"的提醒，不是要恢复的记录。
- 提醒里没有正文：`TerminalAttention` 只有两个布尔值，没有命令行、输出或凭证字段，因此标记不可能复述会话内容。
- 断开不是提醒：会话结束仍由 `SessionState` 表达，标签显示状态词，头部下方（终端之上）常驻原因与下一步；它不会自己消失，
  也不会在别的标签里冒出来打断输入。关闭提醒不改变响铃字节、命令块、会话状态或断开原因。
- 开关：设置页"后台提醒"一组，响铃与命令结束各一个"开/关"，可分别关闭；关闭后不再标记，已经带上的同类标记同时消失。
  两个开关随 `preferences.json` 的 `bell_notice` / `command_notice` 保存（缺失时都算开），重启后继续生效。
- 减少动态效果：标记是静止文字，没有任何过渡或闪烁，因此"减少动态效果"下无需额外处理，两种设置下都完整可见（R3、R9）。

### 提示（toast）

一次性的结果用右下角的 toast 告知，不占页面空间：文件改动成功、上传/下载完成、开始上传、复制路径、
上传失败。成功约 4 秒、问题约 9 秒后消失，指针停在上面时暂停，可随时关闭，最多同时 3 条。色调与图标
与状态标签、横幅一致（成功 ✓、失败 ×）。需要用户作答或需要逐项说明的内容（确认、部分失败的改动明细）
留在原处，不进 toast。

### 文件视图（SFTP）

两栏都有右键菜单：条目上为打开（文件夹）/ 复制到另一侧 / 下载到… / 重命名 / 复制路径 / 删除，空白处为
新建文件夹 / 上传文件…（服务器侧）/ 刷新 / 复制当前路径；复制的本机路径为系统写法（`C:\…`）。条目的图标
按钮只在悬停与选中时显示。从系统文件管理器把文件拖到服务器一侧即上传到当前目录。窄窗口才显示
"本机 | 服务器"切换条；传输条只在本会话有传输或有无法开始的传输时出现。

**跟随终端目录（R15，`view.rs::files_follow_button`）。** 服务器栏头部工具栏里、紧挨"刷新"之后有一个小图标开关
（`files.follow`，`Icon::Follow`：圆环加四向刻度的"定位"线条图标，24 格、1.6 线宽，与图标集同一画法）。跟随时它坐在
`accent tint` 底上、图标用 `accent on tint`（`components::toggle_icon_button`），关闭时是普通的图标按钮。它不再占面包屑
下方的一整行：原来的分段开关与黄色说明行（`files.follow.row` / `files.follow.note`）都已删除。状态和原因只在悬停提示里
（`files_follow_tooltip`）：先写状态（"跟随终端目录：开 / 关"），需要时再接 " · " 和原因——终端还没报告目录、
因手动浏览已关闭、报告的目录读不了（附路径，仍显示上一个可读目录）；正在跟随且已有报告时只写状态。它仍是文件栏的
键盘停靠点（`FilesStop::Follow`，焦点环画在按钮自己身上），Enter 与 Space 切换，与点击走同一段代码，切换后环留在
原处。测试：`the_file_view_follows_the_directory_the_shell_reports`（在刷新旁、属于头部、图标尺寸、没有说明行）、
`a_directory_that_cannot_be_read_keeps_the_last_readable_one_and_says_why`（原因在悬停提示里）、
`the_follow_switch_is_a_keyboard_stop_and_answers_enter`（Enter 与 Space）。

外观（R6）：右键菜单是共享的 `menu_card` + `menu_entry`（删除为危险项）；行首的标记方框是共享的 `checkbox_box`
（整格 24×24 指针目标，悬停整格时方框变 accent 边）；多选横幅是一行：左边"已选 N 项"与操作说明（可换行，不再被
省略成"…"），右边四个动作按钮并排，栏太窄时整组换到下一行，整组仍放不下时按钮在栏内继续逐个折行（动作组可收缩到栏宽、
不设最小宽度；`flex_none` 的整组会保持单行宽度而冲出栏的右边）。按钮之间的间距是每个按钮自己的右外边距，不是组上的
`gap`：随文字缩放的 `gap` 不计入换行组的自身内容宽度，组因此把本可放下的最后一个按钮折到下一行，而栏仍按一行算高，
按钮就画到了栏的下边界之外（720 宽、115% 时实测）。现在栏随它真正占的行数长高，720 宽、115% 下四个动作仍在同一行且都
在栏内；测试 `the_selection_bar_keeps_its_actions_inside_it_at_the_narrow_window` 断言每个动作都在栏内与窗口内，
`the_selection_bar_lays_its_actions_in_a_row_inside_it` 断言四个动作并排、都在栏内，
而不是每个按钮各占一整行；读取失败是共享的 `card`，
头部是危险色调的图标块与一句"无法读取此目录"；SCP 问题里"递归复制"是共享的勾选框（仍是键盘停靠点，焦点环不挪位）。
**行内问题仍留在栏内**（新建文件夹、改名、删除、上传 / 下载确认）：它是该栏键盘停靠序列的一部分（`files_question_stops`），
只挡住自己那一栏，另一栏与终端照常可用；改成全窗口的 `modal` 会改变这两条交互语义（R6 要求不变）。外形见下文 T7。

**新视觉（T7，ADR-7）。** 结构、键盘顺序与选择器不变，只换外形：
- **栏头是 `components::toolbar`**（`files.header` / `files.local.header`：最小高 44、底部发丝线、可换行）。左边是 28 见方的
  `leading_icon`（服务器 accent 色调 / 本机中性色调，`files.source`）、名称（title 字重，`files.host`）与等宽的目标，
  右边是工具（上一级、刷新、跟随、新建文件夹、上传 / 下载）。栏头贴着栏的两边，内容在它下面留 12 的边距。
  窄窗口的“本机 | 服务器”切换行也是一条 chrome 层的 `toolbar`（新增选择器 `files.toolbar`）。两栏之间、终端与文件之间
  是 `components::column_divider`。
- **面包屑**（`files.path`）不再是带边框的输入框样式：行首一个文件夹图标，每段是 `components::crumb`（次要文字、
  悬停浅底与主文字色，当前段主文字 + 500 字重），段间是 caption 大小的 `crumb_separator`。每段仍以 `CRUMB_ROOM`
  为上限、段内换行、整体换行、提示完整路径。
- **筛选框**是带放大镜的搜索框（`TextField::set_leading_icon(Search)`）。
- **列表**不再装在 surface 卡片里：表头（紧凑 28 高、500 字重、底部发丝线）和行直接铺在页面上。每行（含“上级目录”）是
  `components::compact_row_frame`：与 `list_row` 同一张面表，高一个紧凑控件（28）。键盘所在行是 Selected
  （`SURFACE_SELECTED` + 左侧 accent 竖条，改版前是 accent tint），批量标记行是 `ACCENT_TINT`（改版前是 selected 底），
  与首页列表一致；其余行只在悬停时亮一档。大小列宽 `FILES_SIZE_WIDTH`。
- **读取中**是一句 muted 说明加 `FILES_LOADING_ROWS`（3）行 `skeleton_row`（`files.loading`）；**空目录**是居中的
  accent tint 打开文件夹图块 + 状态 + 下一步（`capped_text`，`files.empty` / `files.empty.hint`）；筛选无结果是放大镜 + 一句。
  等待服务器应答的提示（`files.pending`）是警告色调的 `inline_banner_frame` 加可换行的句子。
- **行内问题是 `components::inline_form`**：raised 底 + 强发丝边（危险 / 警告问题为该色调的边）+ 圆角 8 + Raised，
  头部是 `form_heading`（动作图标在色调 tint 上 + 一句，`files.question.heading`），字段上方是 `form_label`，协议分段只有
  自己的宽度。新建文件夹 / 改名 / 删除仍以等宽字体写出目标的完整路径（`files.question.target`）；上传 / 下载的远端路径
  已在“远端目录 / 远端路径”输入框里，**不再在下方重复**——只有上传时“目录 + 所选文件名”得出的最终路径与输入不同，才以
  MUTED 小字写“将保存到 …”（`transfers_lands_at`，同一选择器；显示与不显示两种条件见
  `an_upload_says_where_it_lands_only_once_a_file_names_it`）。按钮行占满表单宽度、靠右，**取消在左、主按钮在最右**，
  键盘停靠顺序随之是“…… → 取消 → 确定”（`files_question_stops`）；传输覆盖确认是“保留 → 替换”。
- **按钮顺序（全应用，T7 起）**：一组回答按钮靠右排列，主按钮（Primary / Danger 的那一个）在最右，取消 / 拒绝 / 保留
  紧挨在它左边；Tab 顺序与视觉顺序一致。已按此调整：文件栏问题、传输覆盖（栏内与传输中心）、片段编辑器“取消 → 保存”、
  片段删除“保留 → 删除”、连接表单里的主机身份确认“拒绝 → 信任”与 ProxyCommand 确认“拒绝 → 运行”；隧道表单、停止 /
  删除 / 暴露确认、远程桌面证书卡、备份对话框与批量预览本来就是这个顺序。T8 的对话框三段式沿用同一规则。
- 栏的焦点环改为 `FOCUS_RING`，边框平时就在（透明），拿到键盘时内容不再挪动 1px。
- 拖动文件时跟随指针的标签是 raised + 强边 + Overlay，尺寸取窗口正在用的缩放。
- 截图：`files-dual`、`files-question`（新增）、`files-marked`、`files-server-failed`、`files-empty` 各有深浅、中文与
  旧 720×480 @200% 变体（已取消）；`files-split` 只在宽窗口出现，保留深浅与 1680×940 中文。

### SCP 文件复制（`crates/ssh/src/scp.rs`、`transfers.rs`）

传输有两种协议，默认 SFTP，另一种是传统 SCP。选择协议是用户的事：主机上存一个默认值（新建主机弹窗的
"协议"分段，`HostConfig::transfer_protocol`，存成 SFTP 时**不写字段**，旧记录字节不变），单次传输还能在
上传/下载问题里再改一次（`FilesDialog::set_protocol`），只作用于这一个任务。两种协议都不互相兜底：
SCP 失败就报 SCP 的失败，SFTP 失败就报 SFTP 的失败，不会静默换一个重试。

- 传输问题现在有两个输入框：远端目录/远端路径（可编辑，`files.question.remote`）与本地路径/本地目录。
  前者预先填好视图当前目录，没有列表时（`BrowseFailure::Unavailable`）预填 `~`——那是唯一不靠列表就能知道的
  远端位置，也是用户可以改的那一个字符。SFTP 不可用时两个方向都能手输，这就是"目录浏览需要 SFTP，但仍可用
  SCP 复制文件"的路径：工具栏在上传旁给出下载（`files.download`），失败卡同时给出上传与下载
  （`files.failure.scp.upload`、`files.failure.scp.download`），并写明"列表与管理需要 SFTP，复制文件仍可用
  SCP：打开上传并填写远端目录（例如 `~`），打开下载并填写远端文件（例如 `~/report.txt`），再选择 SCP"
  （`files.failure.scp.hint`）。这句给两个方向各自的例子：上传那一栏要的是目录，下载那一栏要的是文件，
  共用一个 `~/report.txt` 会让上传方向把一个写着没错的目录填成文件。只做上传方向、把下载留给
  列表里的条目，等于在无 SFTP 的服务器上少了一半能力，因此两个入口都在没有列表时出现；有列表时下载从条目
  行开始，工具栏不重复它。整条路径也只用键盘就能走完（R4）：`files.upload` / `files.download` 两个动作
  （`Ctrl+Shift+U` / `Ctrl+Shift+D`，macOS `⇧⌘U` / `⇧⌘D`）在没有打开文件视图时会先打开它并把键盘放到服务器
  那一栏，然后问同一个上传 / 下载问题；面板获得焦点后 Page Up/Page Down 翻、Home/End 到头，Tab/Shift+Tab 在
  该栏自己的控件之间走（工具栏的上传与下载、失败卡的两个手输入口、问题里的本地框、远端框、SFTP、SCP、递归、
  确定、取消，最后是头部第二行里切回"终端"的那一段），Enter/Space 激活当前控件，Esc 先关掉问题并把键盘还给这一栏、
  再按一次才关掉整个文件视图（终端拿回键盘，`reclaim_focus`）。切回终端因此有两条键盘出口，一条在焦点环上
  （Tab 到"终端"分段、Enter 按下）、一条是 Esc；文件视图打开时终端列被它替换，没有出口的话键盘就被留在里面了。
  失败卡比一屏高时因此不必滚轮：115% 文字、720 宽下原因与两个手输入口用 Page Down/Tab 就能到达并被激活，
  焦点环画在控件自身（accent 边框，与设置页焦点环一致）。
- 焦点环画在控件自己身上，所以每个停靠点都要把**自己那一行**转进栏的视口，而不是只把失败卡翻到底：工具栏停靠点
  回到栏头（栏内容的第一行），问题里的每一行（本地框、远端框、协议、递归、确定/取消）与打开问题时自动聚焦的
  第一个输入框都按上一帧画出的真实 bounds 计算最小滚动量，已经在视口里就不动，比视口还高的一行按顶部读。
  框的高度取决于它带的文字、错误原因和选项（SFTP 换成 SCP 会多出说明与递归两行），所以位置的来源是"画它的那一帧"
  （`on_children_prepainted` 记录内容坐标），刚打开的问题在那一帧结束后（`defer_in`）再转一次并重画：720×480
  加 115% 文字时 `files.question.input`、问题末尾的取消按钮、栏头的上传/下载都不会落在视口之外。
  头部第二行里那条切回终端的停靠点画在这一栏之外，永远不会被栏的沿挡到，因此不需要转。
- 传统 SCP 由服务器自己的 `scp` 完成，走同一条已认证连接（含跳板链）上的 exec 通道：上传是 `scp -t`（服务器当
  接收端），下载是 `scp -f`（服务器当发送端），控制记录就是这个通道的字节流。OpenSSH 9 的 `scp` 默认说 SFTP，
  只有 `-O` 才回到传统协议，而 OpenSSH 8 不认识 `-O`；哪一个由每个任务开始时的一次探测决定，两种结果对用户
  都是同一个"SCP"，不是两种协议。
- 路径是数据：所有远端参数在进入命令前都按 POSIX 单引号规则转义（`'` 写成 `'\''`），开头 `~/` 独立替换成
  `"$HOME"`，因此空格、中文、引号、分号、`$(…)` 都只是路径，不会变成第二条命令。上传的 `-t` 目标目录同样
  按用户写的字符串取父目录（`parent_directory`），不先规范化：`~/报告.txt` 的父目录是 `~`，交给远端 shell
  展开成真实 home，而不是被规范化成根目录下名叫 `~` 的目录；存在性探测与落点因此指向同一个地方。
- 递归复制必须由用户勾选（`files.question.recursive`，只在协议为 SCP 时出现；`Transfer::with_recursive`）。
  没勾选时源是目录就在窗口里被拒绝（原因写在字段旁），不会悄悄复制整棵树；协议为 SFTP 时源是目录则直接说明
  "只有传统 SCP 能复制目录，请把协议改为 SCP 再勾选递归复制"（`PathProblem::DirectoryNeedsScp`），而不是把
  目录说成"没有文件名的路径"。SCP 只复制被点名的文件或目录：列表、新建、重命名、删除仍然只有 SFTP 能提供。
- 没有续传：SCP 任务不产生检查点（`Transfer::resume_hint()` 对它返回 `None`），列表里不出现"续传"和已确认
  偏移，重试就是从头再复制一次；即使有旧记录带来偏移，也会在写任何字节前被拒绝（`ResumeRefused::Unsupported`）。
- 进度只能是真进度：单个文件知道总字节数，百分比才是真的；递归下载在收到最后一条记录前无法知道整棵树多大
  （SCP 不能列目录），所以树下载的 `Progress` 不带 `total`，传输条显示已写字节而不显示百分比，完成状态只在
  最后一个文件之后给出。绝不把当前文件的大小当作整棵树的总量——那会画出"100% · 12/5 B"这种自相矛盾的数字。
- 写入边界：上传直接写目标名（协议没有 rename），所以取消或失败后目标是"含部分内容的文件"，传输条照实写出
  这个路径；下载先写目标旁的 `.shellune-part-<id>` 再改名，因此同名目标在回答确认前完全不变。远端同名目标
  在写任何字节前先问（存在性用一次带引号的 `[ -e … ]` 探测），保留原文件则任务以取消结束、内容不变。
- 下载只接受请求的那个名字：服务器发来别的文件名（`C`/`D` 记录里的名字）就拒绝，不写任何文件；递归里的
  条目名必须是单层名字（`..`、含分隔符、绝对路径都拒绝）。失败按服务器自己的话分类：权限不足、路径不存在、
  磁盘满、连接断开各自不同，并把服务器给出的文本一并显示。

### 文件、传输与转发的任务连续性（R6）

三件事共用一套语言：来源、目标、方向、进度、结果、下一步。凡是“任务还在做”的地方都只说队列里真的有的东西，
界面不自己编一个比例或一个成功。

**文件双栏与窄窗口。** 宽窗口并排画本机与服务器，窄窗口一次画一侧（“本机 | 服务器”分段切换）。两侧是两个
独立对象：各有自己的路径、自己的选中项和自己的问题。改变窗口宽度只改变“画几栏”，不改变选了哪一侧，也不改变
任何一侧的路径，所以变窄再变宽、或反过来，用户原来在看的两条路径都还在（`a_window_that_widens_and_narrows_again_keeps_the_source_and_both_paths`）。
窄窗口里分段条右侧多一行 `另一侧 · <主机>: <路径>`（`files.other`，截断时提示里有全文）：被隐藏的那一侧现在
在哪仍然读得到，这就是“改变呈现方式不丢失另一侧路径”的界面部分。

**长路径。** 面包屑一段一个文件夹，每段有上限宽度（`CRUMB_ROOM` = 240 逻辑像素 × 缩放）：名字本身在段内换行
读全，不被省略号截断；段多了就整体换行，最后一段（当前目录）也在列内。每段的提示给出它指向的完整路径，最后
一段的提示给出当前完整路径。这样 115% 文字、720 宽下，深目录仍然逐段可读，而不会把列表挤出窗口——
`a_deep_path_is_readable_instead_of_being_cut_off` 保留，新增 `a_deep_path_is_drawn_inside_its_column_at_the_large_text`
把“每段都在列内、单段有上限”也固定下来。

**空、失败与无权限。** 空目录是状态而不是空白列表：图标 + “这个目录是空的” + 下一步（把文件从另一侧拖过来，
或用上面的新建文件夹；`files.empty` / `files.empty.hint`）。失败卡写明原因（失败自己的话）、这一侧与终端是两件事
（`files_terminal_note`）以及可执行动作（刷新 / 上一级 / 重连，视原因而定）；权限不足与路径不存在是两个不同的
原因（`BrowseFailure::PermissionDenied` 与 `NotFound`），本机与服务器也各用自己的说法，不把本机目录说成“服务器”。

**危险操作。** 删除目录/文件先问（`FilesDialog::Delete`，写明完整路径、种类、目录内容会一起删除）；重命名为已存在
的名字、上传落到已存在目标同样先问；问题存在时不发任何请求，取消就是零写入。

**传输。** 每行来自 `TransferQueue`，状态词就是队列的状态：`排队中`（Queued）附一句“已交给连接排队，还没有开始
传字节”（`transfers_queued_note`），`进行中`（Running）、`等待你的选择`（AwaitingOverwrite）、`已完成`、`失败`、
`已取消`。排队不画比例，也不写“进行中”。

- 比例只在知道总量时出现：`Running { total: Some(_) }` 才画进度条（轨道与填充用 `LOADING_TRACK` / `LOADING_FILL`
  两个既有 token，不新增颜色），填充宽度等于队列给出的千分比。总量未知（例如整棵树下载）只有已写字节与速度。
- 速率与剩余是测量值：窗口在应用了字节的那一帧读一次时钟，两个采样（间隔 ≥ 400ms）才有速度；采样间没有新字节
  就记 0（停滞不保留旧速度），因此不会出现“卡住却还在倒计时”。速度低于 1 KB/s 不显示估计——那个数只能给出几小时
  的假精确。剩余时间按 (总量 − 已写) / 速度 计算，用 `剩余 0:08` 这种时钟写法（`transfers_remaining`），不依赖语言里的复数。
- 失败与停止：失败行给出原因、留下的部分文件（`transfers_residue`）与已确认偏移（`transfers_resume_from`）；有检查点
  时提供“续传”（`transfers.resume`，新任务携带检查点，worker 仍会先核对），没有则提供“重试”（`transfers.retry`，
  从头开始）；两者同时可用时另有“重新传输”（`transfers.restart`，明确丢弃检查点）。“停止”立即把任务标为已取消并
  通知 worker，之后 worker 的报告只更新留下的部分文件，不会把已停的任务改回进行中。
- 没有“暂停/继续”：worker 与队列都没有暂停语义（`TransferCommand` 只有 Start / Decide / Cancel），本轮不给一个只会改
  样式的按钮。真正的暂停需要 `crates/ssh` 与 `crates/core` 的改动，超出本轮范围，记为未实现。
- **新视觉（T7，ADR-7）**：传输条的标题行是它自己的工具行（传输图标 + “传输” + 运行中的数量，仍在滚动区之上）；
  每个任务是 panel 底 + 发丝边 + 圆角 6 的一行，状态句与动作同在第一行，条太窄时动作整体换成单独一行并放在句子**上面**（`flex_wrap_reverse`；句子最窄
  `FILES_SELECTION_WORDS_MIN`，不再被挤成一行两个字）——720×480 @115% 下传输条按视口 42% 封顶，长原因被截在下方可滚动，
  “续传 / 重新传输”仍在首屏；进度条是共享的 `components::progress_bar`（确定）；“无法开始”与
  “文件夹未传输”是色调 tint 上的状态记号 + 句子；覆盖确认是危险边的 `inline_form`。传输中心（传输对话框）的
  行改为 `list_row_frame`：左侧上传 / 下载图标在状态色调上，右侧状态 chip；没有任务时是共享的 `empty_state`
  （`transfers.empty`，取代原来的 `state.card`）。

**端口转发。** 每行写清四件事：类型（本地 / 远程 / SOCKS）、经由哪台主机、两个端点各自的所在侧、当前状态。端点方向
按类型写成句子而不是只列两个地址：本地转发是 `本机 127.0.0.1:8080 → 服务器 db.internal:5432`，远程转发反过来是
`服务器 127.0.0.1:9000 → 本机 127.0.0.1:3000`，SOCKS 是 `本机 127.0.0.1:1080 · SOCKS`——同一串端口号在两种类型下
含义相反，只有写清两侧才不误导。状态是 `已保存` / `启动中` / `监听中` / `已停止` / `失败`；失败行写出原因：端口被别的
程序占用、没有权限绑定、服务器拒绝、目标不可达都译成界面语言并带上具体端点（`fwd_failure_*`），未知原因照原文显示
而不是吞掉。

- 停止与删除都先问（`forwards.confirm`）：问题里写出这条隧道（类型 · 主机 · 所在侧的监听端点）和答案的后果——停止
  只关闭监听、规则保留；删除会从规则文件移除，正在监听时先停止。取消不改变任何状态，也不向连接发命令
  （`stopping_or_deleting_a_tunnel_asks_first_and_the_answer_is_what_changes_it`）。
- 新建/编辑表单本身就是“类型 · 经由主机 · 两个带标签的端点 · 一句话预览”；保存前校验在表单内出现（`forwards.error`），
  被拒绝时表单不关、文件不写（`the_tunnel_form_refuses_an_incomplete_endpoint_and_saves_nothing`）。
- **新视觉（T7，ADR-7）**：表单是 `inline_form`（头部隧道图标 + “新建 / 编辑端口转发”，每个字段上方 `form_label`，端口框宽
  `FORWARD_PORT_WIDTH`，底部取消在前、保存在后右对齐）。“经由主机”不再是每台主机一段的分段控件（主机一多就冲出
  表单），而是一个字段样式的选择器（`forwards.host`：服务器图标 + 名称 + 等宽地址 + 下拉箭头），点开后在其下方展开
  `menu_card`（`forwards.host.picker`）：顶部搜索框按名称或地址过滤（忽略大小写），列表最多 `FORWARD_HOST_ROWS`（6）
  行高、超出滚动（`forwards.host.options` / `forwards.host.option.<n>`，已选主机带勾），无匹配时一句
  `forwards.host.none`。键盘：控件上 Enter / Space / ↓ 打开（标记落在当前主机），↑↓ 移动、Enter 选中并把键盘还给控件，
  Esc 只关列表不关表单，点外部关闭；Tab 从控件进入第一个地址框，第一个地址框 Shift+Tab、最后一个框 Tab 回到控件
  （`the_tunnel_forms_host_picker_narrows_twenty_four_hosts_from_the_keyboard`，24 台主机）；停止 / 删除确认与“监听所有地址”确认是警告 / 危险边的
  `inline_form`（停止图标 / 垃圾桶 / 地球，按钮右对齐）。规则行仍是 `list_row_frame`，文字最窄 `FORWARD_ROW_WORDS_MIN`，
  对话框太窄（720×480 @115%）时命令换到文字下一行，不再压在“监听中”chip 上。

**捕获。** `files-dual`、`files-empty`、`transfers-running`、`transfers-failed`、`forwards-empty`、`forwards-running`
六个场景由夹具数据构造：两侧目录来自脚本化通道（不读磁盘、不连服务器），传输任务通过队列自己的事件进入“进行中 /
失败”，转发规则在不绑定任何套接字的情况下标记为监听中。每个场景都有浅色与深色，T7 起另有浅色中文与
720×480（旧 200% 变体已取消）；T7 新增 `files-question`（上传问题的行内表单）与 `forwards-form`（新建隧道表单），T9 新增
`forwards-host-picker`（隧道表单的主机选择器展开态，`capture_forward_host_picker_open` 判断就绪）；
就绪判断读的是渲染所用的同一份模型（`capture_files_listing`、`capture_transfer_count`、`capture_forward_counts`、
`capture_files_question_open`、`capture_forward_form_open`）。

**选择器变更（T7，供 T9 汇总）**：传输对话框无任务时 `state.card` → `transfers.empty`（测试
`transfers_and_forwards_open_over_the_page_and_close_again` 同步）；远程桌面结束时窗口画的 `terminal.failure` 从卡片变成横幅，
选择器同名（`terminal.failure`、`.next`、`.details`、`.copy`、`.detail`；`terminal.banner` 只在重连中出现）；新增 `files.toolbar`、
`files(.local).loading.skeleton.<n>`（骨架行的动画 id）、`transfers.progress.bar.<n>`、`desktop.status.headline`、
`desktop.status.detail`、`desktop.certificate.fingerprint`；其余文件、传输、转发、桌面选择器不变。
### X11 转发（`crates/ssh/src/x11.rs`、连接表单、会话头）

X11 转发让服务器上的图形程序显示在这台电脑上。三个模式与 OpenSSH 命令行同名：关闭（默认，`ssh` 不加参数）、
受限（`ssh -X`，交给 X11 SECURITY 扩展限制远程程序）、信任（`ssh -Y`，去掉那些限制）。模式存在主机上
（`HostConfig::x11`，关闭时**不写字段**，旧记录字节不变），单次连接可以在连接表单里再改一次，只作用于这一个
连接，不写回主机。

- 默认关闭，且不许被"顺手打开"：主机表单的 X11 分段（`hosts.editor.x11`）选到信任时，旁边立刻出现风险说明
  （`hosts.editor.x11.warning`），连接表单选到信任时同样（`remote.x11.warning`）。
- 信任模式每次连接前问一次（`RemoteStatus::ConfirmingX11`，`remote.x11` 横幅）：三个回答是"使用信任模式"
  （本次连接生效）、"改用受限模式"（保留转发但回到 `-X` 的限制）、"不连接"（什么都不连，表单与已输入的内容
  保持原样）。回答前一个字节都不发：没有任何连接被拨出。风险文字说明它会去掉 X11 SECURITY 扩展，服务器上的
  程序因此可能读取同一显示上其它窗口的按键输入。
- **两种模式用的凭据不同，而且都是真的不同**（`Prepared::credential`）：
  - 受限模式先向本机显示申请一个**不受信任**的 cookie，用的就是 `ssh -X` 的那条命令：
    `xauth -f <本会话的私有文件> generate <display> MIT-MAGIC-COOKIE-1 untrusted`（`xauth` 按 `PATH` 查找）。
    `-f` 指向本会话自己的私有目录（权限 0700）里的一个文件，`XAUTHORITY` 仍然是这台电脑自己的 authority
    文件——`xauth` 用它去和显示服务认证，只把新条目写进私有文件；读回那个 cookie 后私有文件立刻删除。
    这与 OpenSSH 的 `client_x11_get_proto` 一致，而且必须一致：`xauth generate` 会**替换**同一个
    (family, address, display, protocol) 键的条目，而显示自己的条目正好是这个键，条目写进 authority 文件就会把
    用户自己程序的受信 cookie 换成 untrusted 的。所以这台电脑的 authority 文件**只被读取、从不写入**，会话结束后
    逐字节不变，权限模式与信任模式的凭据也各自独立。
    X 服务器因此把经这条授权连上来的远程程序当作不受信任的客户端，SECURITY 扩展才有东西可限制。
  - 有一类显示服务会登记 SECURITY 扩展、对 `xauth generate` 返回授权 id，却不接受它自己签发的这条授权
    （例如 Debian 的 Xvfb 21.1.16：只有写进它自己的 `-auth` 文件里的条目才会被接受）。所以拿到 cookie 之后、
    发出 `x11-req` 之前，产品先用这份凭据向本机显示发一次最小的连接 setup 请求（`Prepared::confirm_restricted`，
    与桥接改写后交给显示的请求同一形状、同一填充规则）：显示接受，会话才继续；被拒时**不进入"可用"**，而是给出
    独立失败原因 `X11Failure::RestrictedUnsupportedByDisplay` 与下一步（改选信任模式并确认风险，或换用会执行
    SECURITY 的显示服务 Xorg/XWayland/XQuartz）。不这样做的话，会话头会报"可用"，而用户要等到第一个远程程序的
    终端里才看见显示服务的 `Invalid MIT-MAGIC-COOKIE-1 key`。这也意味着受限模式的"可用"只在显示服务真的接受
    它签发的授权时出现——本机是否具备这样的显示服务是环境事实，交付证据里如实记录，不拿信任模式替代。
  - 信任模式直接用 `XAUTHORITY` 里这个显示自己的 cookie，不申请、也不改写任何东西，所以它不需要 `xauth`。
  - 受限模式拿不到不受信任的 cookie 时（本机没有 `xauth`、`xauth` 失败或超时、显示服务不支持 SECURITY 扩展）
    给出**独立失败原因**（`X11Failure::NoUntrustedCookie`）与下一步（装 `xauth`／换支持 SECURITY 的显示服务／
    改选信任模式并确认风险），**绝不退回用受信 cookie 转发**：那会把默认模式悄悄变成它本来要避免的风险。
- 显示名按平台解析（`parse_display`）：`:N`、`host:N`、`tcp/host:N` 之外，macOS 上 XQuartz 用 launchd socket
  路径命名显示（`/private/tmp/com.apple.launchd.XXXX/org.xquartz:0`），凡是 host 部分含路径分隔符的就按本机
  unix socket 直接连接，并按本地显示查 cookie，而不是把它当成 TCP 主机名。
- 请求发生在 shell 之前（`x11-req` 必须在服务器准备会话环境之前到达，否则远端 `DISPLAY` 不会被设置）。本机这一侧
  的检查全部在请求之前完成，顺序是"显示能不能连 → 能不能申请到受限 cookie → 显示接不接受这份 cookie"，
  所以本机的显示问题会先于服务器侧的拒绝被报出来（服务器侧的原因在信任模式下照旧可区分，见下）。请求被
  接受后才把本机显示桥接起来；请求被拒绝、本机没有显示服务、本机没有 cookie、本机无法申请受限 cookie、
  本机显示不接受它自己签发的受限 cookie 时只产生"X11 失败"，SSH 会话照常可用，终端不会被判为断开，也不会被
  标成"X11 可用"。
- 本机 cookie 不出门：每个会话生成一个随机的代理 cookie 发给服务器（`MIT-MAGIC-COOKIE-1`），远端程序用它连回
  来，桥接在这一侧把请求里的代理 cookie 换成上面那份凭据再交给 X 服务器（`rewrite_setup`）。据此同一会话里
  的第二个程序不需要第二次认证，服务器也从未拿到本机的 cookie。
- 失败原因是分开的，因为修法不同（`X11Failure`，每条都有自己的原因与下一步）：本机没有 `DISPLAY`、authority
  文件里没有该显示的 cookie、显示服务连不上、**本机申请不到不受信任的 cookie**、**本机显示不接受它签发的受限
  cookie（显示服务的 SECURITY 扩展只登记不执行）**、**服务器没有可用的 `xauth`**、
  **服务器有 `xauth` 但仍然拒绝（多半是 `X11Forwarding no` 或 `XAuthLocation` 指向跑不起来的程序）**、桥接中途
  断开。服务器对 `x11-req` 的拒绝不带原因，所以在同一条已认证连接上做一次**尽力而为的探测**
  （`probe_remote_xauth`：一个独立 channel 上执行 `command -v xauth` + 可执行性检查，限时）：能证明"远端没有
  可用的 xauth"就单独给出这一条与"装 xauth"的修复方向；能证明 xauth 在位就说明是转发配置（多半
  `X11Forwarding no`，或 `XAuthLocation` 指向跑不起来的程序）；探测不出结论时保留合并说明并**明说服务器没有提供
  原因**，不猜。
- 头部第二行（`session.context`）用一个小 chip 说明当前状态，四种状态各有自己的选择器：`session.x11.off`（关闭，中性）、
  `session.x11.enabling`（启用中，warning）、`session.x11.ready`（可用，positive）、`session.x11.failed`（失败，
  danger）。chip 的 tooltip 写原因和下一步；失败时**同一段原因与下一步还会作为常显文本块**（`session.x11.notice`，
  里面是 `session.x11.next` 与 `session.x11.reason`）出现在头部下方、终端之上——tooltip 只有指针能读，键盘用户读到的
  是这块文本（R4）。文件视图打开时不画这块文本，失败 chip 留在头部第二行，回到终端才再画：720×480、115% 下文本块
  若留在文件窗格上方，会把失败传输的原因和 Retry 顶出客户区。一个没桥接成功的会话仍然是一个能用的会话，断开或关闭会话时 chip 与文本块随桥接一起消失。
- 这块常显文本的高度上限是窗口高度的 35%（`X11_NOTICE_SHARE`），超出部分在框内滚动（`session.x11.notice.body`，
  与传输列表同一套 `max_h` + `overflow_y_scroll` 写法）：原因与下一步都随文字大小放大，720×480 加 115% 文字时
  两者加起来比整个窗口还高，按内容自然高度绘制会把终端压到零高、并把最后一行画到窗口外。**下一步排在原因之前**
  （`session.x11.next` 是框内第一段、用主文本色，`session.x11.reason` 用次要色跟在后面）：框内滚动只有滚轮，
  纯键盘用户读到的就是首屏那一段，所以首屏给的是"能做什么"而不是诊断。720×480 加 115% 文字下首屏是下一步的**开头**：
  这段动作文本本身约五行高（实测 `session.x11.next` 194px，框高 168px），首屏放下约四行，剩下的它与原因一样靠滚轮
  到达；框高上限不变，终端与关闭按钮留在窗口内（R4）。普通窗口里这段文本仍按内容高度整段显示，不出现滚动。
  tooltip 里仍是原因在前、下一步在后，因为它没有首屏限制。
- **受限模式的交付状态**：受限模式要求本机显示服务**真正执行 X11 SECURITY 扩展**，即接受并强制它自己经
  `xauth generate … untrusted` 签发的授权。Windows 侧的两种显示服务都不满足：Debian 的 Xvfb 21.1.16 登记
  SECURITY、接受 `xauth generate … untrusted`，却拒绝自己签发的这份凭据（只有写进它自己 `-auth` 文件的条目才被
  接受，Owner 已独立复现）；WSLg 的 Xwayland `:0` 没有 SECURITY 扩展。所以 Windows 侧能证明的只有：受限模式在
  这样的显示上**不冒充可用**（`X11Failure::RestrictedUnsupportedByDisplay` + 下一步，SSH 会话照常可用），以及
  信任模式（`ssh -Y`，经风险确认）在同一台机器与本机显示之间端到端可用。
- 受限模式的显示端到端在 `192.168.0.109`（Debian 13）的 `:1` 上跑过（2026-11-10）。该显示的 `xdpyinfo` 报
  `X.Org version: 21.1.16`，进程是 `Xtigervnc`（不是另起的 Xvfb），登记并执行 SECURITY：
  `xauth generate :1 MIT-MAGIC-COOKIE-1 untrusted` 签发的 cookie 能连上。
  `SHELLUNE_X11_DISPLAY=:1`、`SHELLUNE_X11_AUTHORITY=/root/.Xauthority` 下，产品测试
  `cargo test --test ssh_x11 --all-features -- --include-ignored --nocapture` 为 9 passed、0 failed，没有 SKIP，也没有
  受限路径的 GAP。受限用例走 honoured 分支：`X11Status::Ready { display: ":1" }`，同一会话里第二个程序仍只有一次
  SSH 认证，`xlogo` 使本机窗口数从 22 增到 23，会话结束后显示上的客户端数回到接入前的 11。
  `/root/.Xauthority` 的 md5 在整组测试前后都是 `98dbe6df6de7b4ec4950f77524d1240c`，`/tmp` 下没有留下
  `shellune-x11-*` 凭据目录。这台机器上用来编译的是已安装的 stable（rustc 1.99.0）；仓库钉的 1.99.1 工具链当时不完整。
- 同一次 Linux 构建修掉了一个只有 Linux 才会编译到的缺陷：abstract-socket 分支（`cfg(target_os = "linux")`，见
  `crates/ssh/src/x11.rs` 的 `connect_abstract`）原来把 `std::os::unix::net::SocketAddr` 传给只接受
  `tokio::net::unix::SocketAddr` 的 `tokio::net::UnixStream::connect_addr`，Linux 上整个 crate 编译失败；现在先用
  `std::os::unix::net::UnixStream::connect_addr` 连接，再 `set_nonblocking(true)` 后交给
  `tokio::net::UnixStream::from_std`，失败仍按原来的"显示连不上"报出。Windows 与 macOS 不编译该分支，所以此前
  的本地构建看不到它。`a_linux_abstract_display_name_becomes_an_address_and_an_unserved_one_fails` 在该机上通过，
  覆盖抽象地址的构造与"无人监听即失败"。

- 关闭会话先停止接受新通道，再关掉既有桥接：不留下监听，也不留下仍在转发到本机显示的连接。
- 对话框里的控件复用同一套分段与横幅；连接表单里的 X11 选择是一行（标签 + 三个分段，说明在 tooltip 里），
  因为同一个对话框下方还有其它正在进行的连接，长段落会把它们挤出小窗口。主机表单整页可滚动，X11 分段与确定
  按钮一样通过滚动到达，不被裁掉。

## 3. 状态语言

状态先由**形状**和**词**表达，色调只负责同意：每个 `Tone` 有一个固定的标记形状
（`theme::StatusMark` → `Tone::mark()`），同样的状态在 chip、横幅、状态卡和状态栏里画同一个
形状。截图转成灰度、或在色觉障碍下，状态仍然可读。

| 色调 | 形状 | 词例 |
| --- | --- | --- |
| Positive | 实心圆（●） | 已连接、已完成 |
| Warning | 空心环（○） | 连接中、等待确认、加载中 |
| Danger | 实心方块（■） | 失败、被拒绝、凭据不可用 |
| Planned | 空心方块（□） | 已关闭、本版本不提供 |
| Accent | 竖条（▍） | 当前对象 |
| Neutral | 横线（—） | 空闲、没有正在运行的对象 |

一个分区只能处于四种状态之一，映射集中在 `workspace::SectionState`：

| 状态 | 色调 | 呈现 |
| --- | --- | --- |
| 加载中 | warning | 居中状态卡片；头部标题旁有"加载中"chip（空心的环 + 词） |
| 失败 | danger | 居中状态卡片 + 原因 + 建议下一步；头部标题旁有"失败"chip（方块 + 词） |
| 空 | neutral | 居中状态卡片，以该分区的图标为徽记；下一步就是头部主按钮（本地终端页为欢迎页） |
| 有内容 | - | 行列表；头部摘要行写出数量（"2 个会话"） |

规则：

1. 只有"有内容"可以表现为成功；"已就绪"不再单独做 chip，数量即是结果。
2. 会话色调只由 `SessionState` 决定，`Connected` 是唯一 Positive；导航点只在其中有已连接会话时为绿色。
3. 提示、失败、确认问题都用同一个 `banner`：色调底色 + 左侧 3px 色条，文字保持主文字色；需要自带
   标记与标题时用 `notice`（同一个块 + 标记 + 标题 + 一行说明）。
4. 交互状态与状态色调是两套语义，不混用：`ControlState`（静止 / 悬停 / 按下 / 选中 / 焦点 / 校验
   失败 / 不可用 / 进行中）描述"用户正在对它做什么"，`Tone` 描述"这个东西本身处于什么状态"。
5. 焦点、选中、按下、不可用都不止一种表达：焦点是 2px 边 + 焦点色，选中是底色 + 前导竖条 + accent
   边，按下是更深的填充 + 底边加粗，不可用是禁用底色 + 禁用文字（仍保持 3:1）且不再响应指针。

## 4. 键盘、焦点与缩放

所有快捷键只有一份事实：`src/app/keymap.rs` 的动作目录（`ACTIONS`）。每项给出稳定动作 id、输入上下文、
各平台默认组合键和中英文名称；`key_bindings(Platform)` 只是目录默认值的消费者，`Keymap` 是"默认值 +
用户改动"后真正生效的绑定。命令面板、tooltip、终端工具条提示、终端右键菜单和设置页都从 `Keymap` 读组合键，
因此改键后提示同步变化，不会出现两份互相矛盾的字符串；动作被解除绑定时这些位置不再写出组合键。

| 动作 | Windows / Linux | macOS |
| --- | --- | --- |
| 主机 / 终端 / 设置 | Ctrl+1 / Ctrl+2 / Ctrl+3（设置也可 Ctrl+,） | ⌘1 / ⌘2 / ⌘3（⌘,） |
| 下一个 / 上一个页面 | Ctrl+Tab / Ctrl+Shift+Tab | Cmd+Tab / Cmd+Shift+Tab |
| 新建本地终端（终端内也有效） | Ctrl+Shift+T | Cmd+T |
| 文字大小 | Ctrl+= | Cmd+= |
| 界面语言（终端外） | Ctrl+L | Cmd+L |
| 关闭命令面板 / 菜单 / 提示条（终端外） | Esc | Esc |
| 命令面板（终端内也有效） | Ctrl+Shift+P | ⇧⌘P |
| 关闭当前会话（终端内也有效） | Ctrl+Shift+W | ⌘W |
| 重新连接当前会话（已断开或失败时；终端内也有效） | Ctrl+Shift+R | ⇧⌘R |
| 下一个 / 上一个标签（终端内也有效） | Ctrl+PgDn / Ctrl+PgUp | ⇧⌘] / ⇧⌘[ |
| 直达第 1-8 个标签 / 最后一个标签 | Alt+1-8 / Alt+9 | ⌃1-8 / ⌃9 |
| 上传到当前会话 / 从当前会话下载（终端内也有效） | Ctrl+Shift+U / Ctrl+Shift+D | ⇧⌘U / ⇧⌘D |
| 终端内搜索 | Ctrl+Shift+F | ⌘F |
| 终端内复制 / 粘贴 | Ctrl+C / Ctrl+V | ⌘C / ⌘V |

终端内只占用 Shell 程序不读的组合键：Ctrl+F 是光标前移，因此 Windows / Linux 的搜索取 Shift 形式；
macOS 的 ⌘F 是系统通用"查找"，没有 shell 程序读它，所以直接用 ⌘F。Ctrl+W、Ctrl+P 分别是删词与上一条命令，
同样只取 Shift 形式。
复制 / 粘贴与其它应用一致，使用通用的 Ctrl+C / Ctrl+V（⌘C / ⌘V）：有选中内容时 Ctrl+C 复制并取消高亮，
没有选中内容时 Ctrl+C 原样作为中断信号送给程序；剪贴板为空时 Ctrl+V 亦原样送入。标签的中键单击关闭标签，右键打开标签菜单（复制标签、重命名、辨识色、左移、右移、关闭、关闭其他、关闭右侧）；
批量关闭时有传输进行中的会话不会被静默关闭，而是像它自己的关闭按钮一样先询问。

### 文件栏的键盘顺序（`view.rs::files_stops`）

文件视图的焦点环也在控件自身的边框上（accent 边框 + 选中底色），因为这里的控件是按钮与分段而不是页面元素。
一栏拿到键盘后（点击该栏、从传输动作进入，或 Esc 关掉问题后还回来），该栏自己的控件按页面的阅读顺序排成一圈：

1. 该栏本身（`files.panel` / `files.local.panel`）：列表在它里面，失败卡的说明文字也在它里面；这一栏获得焦点时
   整栏画 accent 边框。Page Up / Page Down 按一屏翻、Home / End 到两端——列表比框高时先翻列表本身，列表没有
   可翻的（失败卡比一屏高）就翻整栏，所以 115% 文字、720 宽下卡片下方的两个手输入口一定翻得到。
2. 服务器栏工具栏的"跟随终端目录"开关（`files.follow`，画在刷新旁，所以排在上传之前）、上传（`files.upload`），
   以及没有列表时的下载（`files.download`）。
3. 打开的问题按它画出的顺序：本地框、远端框、SFTP、SCP、递归（只在协议为 SCP 时出现）、确定、取消。
4. 失败卡上的两个手输入口（`files.failure.scp.upload` / `files.failure.scp.download`）：服务器拒绝 SFTP 子系统时
   它们是唯一能复制文件的入口，所以也是焦点停靠点；Tab 走到其中一个时面板会翻到卡片末尾，焦点环落在能看见的位置。

Tab 与 Shift+Tab 在这一圈里前后走；宽窗口下这一圈把两栏连起来（本机栏 → 服务器栏），所以键盘不用指针也能换栏，
窄窗口只有正在显示的那一栏在圈里。控件拿着环时方向键也按同一次序前后一步（列表自己拿着环时方向键仍是移动选择）。
Enter/Space 触发当前控件——与指针点击调用同一段代码，所以键盘和指针不会做出两种不同的请求。问题打开时第一个输入框
拿到焦点，Enter 在任一输入框里等于"确定"，Esc 关掉问题并把键盘还给问它的那一栏。窗口在双栏 / 单栏之间切换
（拖宽到阈值以下、或放大文字）时 gpui 会在那一帧丢掉焦点，键盘回到终端窗格；`Ctrl+Shift+U` / `Ctrl+Shift+D`
会把焦点再放回服务器栏。

### 设置：分类与子项导航（R16）

设置页（`view/settings.rs`）分成两级：左侧是一级分类，选中的分类下面列出它的二级子项，右侧只画当前子项。
不再把外观、终端、提醒、快捷键、数据和关于挤在同一条长页上，快捷键的动作目录也就不会占掉其它设置的阅读路径。

| 分类 | 子项 |
| --- | --- |
| 外观 | 主题、语言、界面文字大小、动态效果 |
| 终端 | 终端配色、字号、默认 Shell |
| 连接 | 保活与超时 |
| 提醒 | 响铃、命令结束 |
| 快捷键 | 快捷键子页 |
| 数据 | 主机文件、导入 SSH 配置、导出备份、导入备份 |
| 同步 | 同步（WebDAV，单页，见下节“WebDAV 同步”） |
| 关于与更新 | 版本、更新（见下节“更新”） |

- 位置按分类记住（`SettingsNav::item`）：从终端回到外观，落回上次看的那个子项，而不是第一个。每个子项是一页，
  只有当前页的行与控件被画出来；`settings.page.<子项>` 与 `settings.card.<子项>` 选择器跟着当前页变化
  （快捷键子页的卡片是 `settings.keys`）。
- 两栏放不下时（视口窄于 780 逻辑像素 × 当前文字缩放）左侧导航折叠为页面顶部的两条选择条
  （`settings.picker.category` / `settings.picker.item`），每个分类与子项仍然可点到，页面不被裁切（R8）。
- 选中项同时用底色、2px accent 竖条与加粗文字表示，不只靠颜色；最近一次输入来自键盘时另画 accent 焦点环。
- 键盘：不在快捷键子页时 ↑/↓ 换分类、←/→ 换该分类的子项；在快捷键子页时 ↑/↓ 换动作行、←/→ 换该行控件、
  Enter 执行（见下节）。
- 连接分类是可编辑的全局默认（见下节"连接保活与超时"）：页面改的就是下一次连接真正使用的数字，
  保存进 `preferences.json` 的 `connection`，没有单独设置的主机在重启后仍按它连接。

### 备份导出与导入（R5、R6、R9，`view/backup.rs`）

数据分类的“导出备份”“导入备份”两行各打开一个居中对话框；备份文件就是存储层的可移植数据文档
（`ShelluneData`：主机与其它连接、最近与收藏、分组、端口转发、常用命令、已信任指纹、共享偏好，
以及本平台的快捷键、文字大小、侧栏与私钥/串口路径覆盖层）。工作区、批量任务、审计日志不在其中。

- 导出（`backup.export.dialog`）：默认不含密码，并明说“文件中不会写入任何密码”。勾选“包含已保存的密码”
  后出现两栏遮罩口令（可显示），口令为空或两次不一致时以警告行说明原因，“选择保存位置…”按钮置灰且不打开
  系统对话框。按钮打开系统保存对话框，起始于用户主目录，默认文件名 `shellune-backup-YYYYMMDD.json`。
  所选路径已是一份备份时以它为基底，其它平台的段落原样保留。完成后对话框内显示路径、写入的密码个数，
  并分两行列出“凭据库无法读取”的连接与“标记记住密码但凭据库没有”的连接（`secrets_unreadable` /
  `secrets_missing`），然后清空口令栏。
- 导入（`backup.import.dialog`）：系统文件选择 → 读取。损坏、不是 Shellune 备份、由更新版本生成、本机数据
  读不出时，对话框只显示“无法导入这个文件，本机数据没有任何改动”和原因，不写任何文件。文件含密码时先问
  口令：口令错误停在这一步并说明，可改为“不导入密码”继续。预览列出来源（平台、版本、时间）、连接/分组/
  转发/常用命令/已信任指纹的新增·更新·删除数、偏好是否变化（含本机快捷键）、将保存的密码数，已信任指纹
  与本机不同的服务器（逐台列出 `主机:端口` 与两边指纹开头；合并保留本机的，覆盖使用文件的）、将从本机凭据库
  移除的密码（逐个列出连接名：连接被删除或不再用密码登录），“本机不存在的私钥文件”（来自其它平台的路径会标出
  来源平台），以及其余沿用其它平台的路径或串口设备（文件里没有本平台段落时，`fallbacks`）。
- 应用方式是“合并 / 覆盖”分段控件，切换时按该方式重新生成预览。覆盖第一次按下只出现危险提示与“确认覆盖”，
  再按一次才写入；切回合并会撤销这次确认。应用前存储层先把全部数据文件复制到 `snapshots/`，失败时回滚并
  在对话框里说明是否已恢复；预览后本机数据被其它保存改动时不写入，自动刷新预览并提示再确认。
- 应用成功后对话框写出快照位置与在本机保存的密码数；凭据库拒绝移除的密码（`secrets_not_forgotten`）逐个列出。
- 应用成功后重新读取主机、分组、已信任指纹、转发、常用命令与偏好（偏好变化时重新应用主题、语言、文字
  大小与快捷键），界面无需重启即显示导入后的数据；对话框显示原数据快照的位置。
- 首页空状态的欢迎卡在“新建主机”“从 SSH 配置导入”旁提供“从备份恢复”（`hosts.empty.restore`），走同一条导入流程。
- Escape 关闭对话框且不写任何内容。截图场景：`settings-backup-export`、`settings-backup-import-preview`。
- 导出（含 Argon2id 加密与写文件）、口令解锁与应用都在后台执行器上运行，界面不卡；运行中按钮置灰并显示
  “正在保存备份…”/“正在处理…”，应用中的对话框阶段为 `import.applying`。导入口令为空时提示“请输入导出这个备份
  时设置的口令”（导入语义），不尝试解锁。对话框打开或口令出错时，口令栏自动获得键盘焦点；对话框打开期间
  主机页的 ↑/↓/Enter 不作用于背后的列表（`hosts_key` 门控同样覆盖同步冲突对话框）。
- 私钥文件在本机不存在的主机（来自其它平台的备份或同步），在主机列表行的名称旁持续显示警告标记
  “本机未找到该私钥文件”（`hosts.key-missing.<n>`，悬停显示路径），记录本身照常保留；存在性检查结果缓存
  5 秒，导入或同步拉取后立即重查（R9）。

### WebDAV 同步（R7、R8、R9、R10，`view/sync.rs`）

设置的“同步”分类是一页卡片（`settings.card.sync`）：服务器地址、用户名、密码（遮罩，只写入系统凭据库，
已保存时占位符为“已保存，留空则保持不变”）、远端目录（默认 `Shellune`）、自动同步开关（默认开）、
“同步已保存的密码”开关与同步口令（开启后才出现，保存时必填，存本机凭据库）、操作行（保存、测试连接、
立即上传、从云端拉取；有冲突时多一个“处理冲突…”），以及测试结果、表单问题与“上次同步：时间 · 结果”状态行；
失败时再加一行“本地修改已保存。同步会在几分钟内自动重试，下次修改后也会重试”。

- 测试连接用表单当前值（未保存也可），在后台执行器上探测：成功显示“连接成功，目录可写入”，失败分别说明
  认证失败、无法连接（附原因）、目录不可写或地址无效。测试不是同步，不改任何本地或状态文件。
- 同步引擎的调用都是阻塞的，窗口在后台执行器上一次只跑一个任务；运行中再请求的任务进入单个等待槽
  （手动任务替换等待中的自动任务，反之不替换），前一个结束后再开始。备份导入的“应用”与同步任务共用这个
  槽：同步运行中按下应用时，对话框停在 `import.applying` 等它结束再写；导入应用中到来的同步任务同样排队，
  两个写入方不会同时 apply。
- 可能写本机数据的任务（自动/手动检查、从云端拉取、以云端为准）和备份应用，从开始到窗口读回其结果为止，
  窗口自己的保存点（主机/连接、收藏、最近连接、分组、转发、常用命令、已信任指纹、偏好）**不写文件**：修改
  立即显示在界面上，文件保持写入方留下的样子。写入方结束后，先把这些修改按三方合并（开始时的文件、窗口
  当前数据、写入方写完的文件；记录按 id 对齐，两边改同一字段时以本机修改为准）写回磁盘，再读回界面，并按
  普通改动在 5 秒后上传——因此拉取刚写入的远端记录不会被内存中的旧数据整文件覆盖，本机修改也不会丢。
  个别文档合并后不是有效文件时保留写入方的版本，并提示哪些内容需要重做（`LocalData::rebase`）。
  合并的“开始时”取窗口开始暂存时内存中的数据（不读文件）；写入方结束后文件读不出或写入失败时，
  不把内存数据整文件写回，文件保持原样、界面重新读取，并提示需要重做的内容与原因。
- 有暂存修改时关闭窗口或退出应用（`on_window_should_close` / `on_app_should_terminate` / 确认关闭）
  不立即关闭：提示“正在保存修改，随后关闭”，等当前写入方结束（受引擎超时约束）、暂存修改写盘后再关闭
  窗口或退出；因此已显示保存成功的修改与已写入凭据库的密码不会因关闭而失去对应记录。若有修改未能保留，
  取消这次关闭并提示（“Shellune 未关闭，以便你重新进行这些修改”）。没有暂存修改时关闭不等待。
- 自动同步仅在“已保存服务器且自动开关为开”时进行：启动加载完成后拉取一次，之后每 15 分钟检查一次；
  主机/连接、收藏、最近、分组、转发、常用命令、已信任指纹、偏好的保存点调用 `note_data_changed`，最后一次
  改动 5 秒后上传。工作区快照不触发；应用云端数据期间的重新加载不记为改动，不会回传。未配置或关闭自动
  同步时不发送任何请求；关闭后“立即上传”“从云端拉取”仍可用。
- 拉取应用后重新读取主机、分组、已信任指纹、转发、常用命令与偏好，界面即时刷新，并以提示说明结果（被云端
  替换的已信任指纹数、云端含密码但本机未开启同步密码等）。
- 备份导入应用完成后同样调用 `note_data_changed`，自动同步开启时 5 秒后上传导入的数据。
- 失败不阻塞界面、不丢本地修改：状态行与顶栏标记（`topbar.sync`，“同步失败”）显示原因；自动同步开启时
  1 分钟后重试，之后每次失败等待加倍（2、4、8 分钟），最长 15 分钟，成功后复位；下一次改动或定时检查也会
  重试。点击标记进入同步设置。上次运行留在 `sync-state.json` 里的失败原因（引擎的英文句子）按界面语言
  显示：已知错误（认证、无法连接、目录不可写、服务器错误、云端文件不可用、口令等）映射为对应文案并附原文
  详情，未知原因显示“上次同步失败。详情：…”。
- 自动同步时两边都有修改、但至少一边只是记录了最近连接（其余数据与上次同步一致；开启同步密码时含密码集合）：
  不弹冲突框，自动合并（最近连接按主机取较新的一条，其余取有实质修改的一方），先以 If-Match 上传、再写本机
  （写前留 `sync-merge` 快照），界面按拉取同样刷新并提示“已同步，已自动合并最近连接”，状态行为“已是最新”。
  两边都有其它修改、旧版状态文件缺少核心基线、云端密码无法用本机口令打开时，仍按下条提示冲突。
- 冲突（`sync.conflict.dialog`）：说明两边都有修改（首次同步时说明云端已有不同数据）、云端数据来源（平台、
  版本、时间）与三个选项的后果：以云端为准（本机先存快照再替换）、以本机为准（云端文件先存到本机快照再
  上传）、取消（两边不变，冲突保留，顶栏显示“同步冲突”）。Escape 等同取消。取消后自动检查不再弹出，
  手动检查或“处理冲突…”会再次询问。冲突决定的快照（`sync-use-*`）与其它快照分开各保留 10 份，自动拉取
  不会把它们挤出。
- 截图场景：`settings-sync`、`sync-conflict`（截图窗口离线，不发送任何请求）。

### 更新（R2、R3，`view/update.rs` + `src/app/update.rs`）

更新器核心在 `crates/update`（清单、下载、minisign 验签、各平台安装路线），调用全部阻塞；窗口只在后台执行器上调用，
并通过 `UpdateBackend` 接缝调用它（应用里就是 `Updater`，测试注入回环 HTTP 服务器上的真实 updater，只替换
Windows 安装器的启动与重启）。

- 阶段（`UpdateStage`）：空闲 → 检查中（区分手动/自动）→ 已是最新（版本 + 检查时间）/ 发现新版本 → 下载中（进度条与
  “百分比 · 已下载/总大小”）→ 已下载待安装（已通过签名校验）→ 安装中 → 待重启（便携版/macOS）；任一步失败进入
  失败（记录失败的步骤，重试从该步重新开始：检查重新读清单，下载重新下载，安装失败且文件仍在时重新安装）。
- 启动：窗口打开时在后台清理上次留下的下载文件和便携版 `shellune.exe.old`；20 秒后若“自动检查更新”为开，静默检查
  一次。自动检查失败不提示（下次启动再试），手动检查失败在卡片上写明原因。开关是本机偏好
  （`preferences.json` 的 `auto_check_updates`，缺省为开），不进入备份与同步文档，拉取或导入不会改它；关闭后启动不发
  任何请求，手动“检查更新”仍可用。截图与默认测试窗口没有 updater，不发请求。
- 设置“关于与更新”分类：版本行（当前版本）+ 更新页（`settings.card.update`）：自动检查开关（`settings.update.auto`）、
  检查按钮（`settings.update.check`）、状态区：
  - 发现新版本：版本号、发布日期、更新说明（过长时按行截断，详见发布页）、“下载并安装”（`settings.update.download`）
    与“打开发布页”（`settings.update.release-page`）；
  - 本机不能自行安装时不提供下载，只说明原因并把“打开发布页”作为主按钮：此安装方式需手动更新（Linux deb 等）、
    开发构建、文件夹无写权限（macOS）、此版本没有适用于本机（平台名）的更新文件、本机的更新文件缺少签名。
  - 失败原因互不相同：签名缺失、没有本机安装包、验签失败（另一把密钥或文件被改）、签名版本不符（写明两个版本）、
    下载中断（未完成文件已删除）、下载超时（服务器长时间不发数据）、检查超时、无法连接、HTTP 状态、清单无法读取、
    安装失败（已恢复原文件）；失败时另写“当前仍是 Shellune x.y.z，未做任何改动”。
- 有新版本时标题栏显示提示芯片（`topbar.update`，“新版本 vX”；窗口窄于 900 × 文字缩放时改为与顶栏按钮同尺寸的
  accent 箭头按钮，完整文字放在提示里，避免把窗口按钮挤出窗口），点击进入更新页。
- 安装/重启前若有活动终端（本地/SSH）、SFTP 传输、远程桌面、串口会话（未结束的会话与未完成的传输），弹出
  `update.confirm.dialog` 列出各自数量；“稍后”停在已下载待安装（或待重启），确认后先保存工作区布局再继续。
  下载完成后若没有活动会话直接安装。
- 若此时同步或备份导入正暂存窗口的保存（DataGate），更新先等待写入方结束、暂存修改落盘后才启动安装器或重启；
  落盘时有修改未能保留（`held_writes_lost_count` 增加）则取消这次安装，停在已下载待安装并提示。
- 路线：Windows 安装版启动安装器（`/S /R`）后关闭所有会话、经与关闭窗口相同的退出路径退出（仍会等待暂存修改）；
  便携版原地替换 exe（旧文件改名 `.old`，下次启动清理），macOS 替换 .app，二者显示“Shellune x.y.z 已安装，重启后生效”
  与“立即重启”（启动新版本后退出）；安装返回“需手动”时打开发布页并说明原因。
- 截图场景：`settings-update-available`、`update-confirm-sessions`（窗口没有 updater，不发请求，不安装）。

### 连接保活与超时（R20）

保活是 OpenSSH 的加密通道探测（`ServerAliveInterval` / `ServerAliveCountMax`）：连接静默一个间隔就发一次探测，
连续这么多次没有回应才判定断开。TCPKeepAlive 是另一件事，本轮不当作替代品，导入时明示"未应用"。

**设置 → 连接 → 保活与超时**（`view/settings.rs`）从上到下是四行加两条说明：

| 行 | 控件 | 说明 |
| --- | --- | --- |
| 保活 | 开关（`settings.keepalive`） | 关就是把间隔写成 0，与 OpenSSH 的 `ServerAliveInterval 0` 同义 |
| 保活间隔 | 秒数输入框（`settings.policy.keepalive-interval`） | 静默这么久之后发一次探测；0 表示关闭 |
| 未响应探测 | 次数输入框（`settings.policy.keepalive-misses`） | 1–100；0 被拒绝，因为"永远不判定断开"不是可达的语义 |
| 连接超时 | 秒数输入框（`settings.policy.connect-timeout`） | 1–600；只覆盖拨号与密钥交换，不覆盖人工确认指纹的时间 |

- 说明行 `settings.policy.window` 把两个数字换算成人能读的句子（"约 90 秒没有任何回应就判定连接断开"）；保活关闭时改说"不会主动发现静默断开的连接"，不宣称仍在检测。`settings.policy.note` 说明没有单独设置的主机都按这里连接。
- 拒绝在原字段发生：`settings.policy.error.<字段>` 画在该输入框下面，值留在框里，文件里的值不动，不静默改成相近的数。负数、非数字、间隔超过 3600、次数为 0 或超过 100、超时为 0 或超过 600 都走这条路；间隔 0 不是错误。
- 关掉再打开保活会恢复上一次用过的间隔，而不是跳回 30 秒。
- 改动写进 `preferences.json` 的 `connection`；从没改过时该块不写入，因此本版本的默认值不会冻结进用户数据。

**主机表单 → 保活**（`view/host_dialog.rs`）是三态分段：`hosts.editor.connection.inherit` / `.off` / `.override`。

- 继承：记录里不写任何字段，表单说明它继承的是哪组数字（`hosts.connection.note`，"继承全局默认：每 30 秒探测一次…"）。旧主机缺字段即继承，改动全局默认会带动它。
- 禁用：记录写成 `keepalive_interval_secs = 0`，只影响这一台；连接超时仍继承全局。说明文字直说"不会主动发现静默断开的连接"。
- 覆盖：三个输入框（`hosts.connection.keepalive-interval` / `-misses` / `-connect-timeout`）以它今天真正生效的数字预填，下面是 `hosts.connection.window` 的判定窗口句子。拒绝同样落在原字段（`hosts.connection.error.<字段>`）；保存被拒绝时表单不关闭，文件里的旧值不变。
- 保存后由 `ChainDraft::with_connection_defaults` 解析成一份 `ConnectionPolicy` 写进真实的 `ChainRequest`：direct 与跳板链的每一跳使用同一份策略，界面显示的数字就是拨号用的数字。

**导入 SSH 配置**（主机列表的导入面板）在每台主机的一行里多一句保活结果（`hosts.import.connection.<行号>`）：两个指令都拿到时写"每 20 秒探测一次，4 次无回应即断开"，`ServerAliveInterval 0` 写"文件对本主机关闭了保活"，只写了一个指令时说"只设置了其中一部分，其余沿用全局默认"。`TCPKeepAlive` 等不支持的指令仍出现在文件级"未应用"列表里，不会假装生效。
### 快捷键子页（`keymap.rs` + `view/settings.rs`）

快捷键是设置页"快捷键"分类下的独立子页，其它设置不再被动作目录挤到下面。页面自上而下是：搜索框、作用范围
筛选（全部范围 / 终端内 / 所有位置 / 终端外，与 `Context::ALL` 同源）、需要回答的横幅，然后每个动作一行。
行内显示名称、"上下文 · 默认/已修改/未绑定"和当前按键（`kbd`）。每行按状态给出操作：修改；动作有绑定时
给出解除绑定；动作不等于默认值（含已解除绑定）时给出恢复默认，因此解除绑定后单项恢复仍然可用。底部给出
"已修改 N 项"与"恢复全部快捷键"。

搜索按中英文名称、动作 id、上下文和当前组合键过滤（`Keymap::matching`），再叠加作用范围筛选；两者一起决定
画出来的行，也决定键盘 ↑/↓ 走过的行（`ShelluneView::key_rows` 是唯一来源，所以环不会停在没画出来的行上）。

- 修改：打开捕获层，下一次按下的组合键成为该动作的绑定。组合键必须带 Ctrl、Alt 或 ⌘（功能键除外），
  否则会抢走终端程序自己读的键；Esc 取消。捕获通过 `intercept_keystrokes` 读键，因此已被别的动作占用的
  组合键也能被看到，而不会先触发那个动作。
- 冲突：只在上下文可能同时成立的动作用之间判定（`shell` 与 `terminal` 重叠，`outside-terminal` 与
  `terminal` 互斥）。冲突不保存，捕获层直接写出双方名称与组合键；保存的绑定彼此冲突时整份回到默认，
  子页用 `settings.keys.conflicts` 横幅写出冲突的组合键与双方动作名。
- 解除绑定：该动作不再占键，组合键交还给终端程序（例如解除粘贴后 Ctrl+V 作为程序按键送入）。
- 持久化在 `preferences.json` 的 `keymap` 里，按平台分开保存（`windows` / `macos` / `linux`），每项记录
  动作 id、上下文和组合键；未绑定的动作写 `keystroke: null`。保存时只替换本平台那一节，其它平台已保存的
  绑定原样保留。写入失败时界面明确说明"本次已生效但未写入"，不冒充已保存。
- 损坏或不兼容：文件无法解析、动作 id 不存在、上下文已变、组合键无法解析或彼此冲突时，整份自定义绑定
  回到默认，设置页顶部用 warning 横幅说明"保存的快捷键未生效"，默认导航照常可用。
- 键盘路径：设置页获得焦点后 ↑/↓ 在动作行间移动（当前行画 accent 焦点环），←/→ 在该行的修改、解除绑定、
  恢复默认之间移动（当前控件画 accent 边框），Enter 执行当前控件，因此不用指针也能改键、解绑和恢复。
  在搜索框里按 ↑/↓ 会先把键盘交给列表，之后的 ←/→ 用于选择控件而不是移动光标。

### 指针目标与焦点

主要图标按钮的命中区域不小于 24×24 逻辑像素：尺寸统一取 `Metrics::hit_target()`（`compact_control_height`
与 24 中较大者），在 90% 文字下也不会缩小。取该尺寸的控件包括：设置页字号按钮、顶栏命令面板按钮与
"新建 / 选择 Shell" 的箭头、终端工具条的命令块视图、搜索、复制、字号按钮、分屏与窗格关闭、分屏分隔条上的
微调按钮、标签关闭（含待启动标签）、头部第二行的关闭、SFTP 文件行的下载 / 回传 / 重命名 / 删除、toast 关闭，
以及密码框的显示开关和路径框的"浏览…"按钮。这些按钮用图标绘制，但作为指针目标对待，因此没有调用点用自己的
小尺寸覆盖它们。

两处布局上的处理需要写明，免得与上面的统一规则读起来矛盾。分屏分隔条的拖拽带仍为 12px 厚：它是连续的拖拽条
而不是按钮，加宽会实打实吃掉两侧窗格的宽度，所以微调按钮按 ≥24×24 命中区绘制并压在相邻窗格的边缘上（分隔条与
窗格都没有 `overflow_hidden`），拖拽带在按钮上下的部分照常起拖。SFTP 文件行的动作列宽按"按钮数 × 命中区 +
间隔"推导（服务器行 4 个、本机行 3 个），表头用同一宽度，因此放大文字时按钮不会溢出到大小列上。

焦点环为 accent 边框，选中的导航项同样画这圈环：底色表示当前页，环表示键盘在这项上。导航项的焦点环只在
最近一次输入来自键盘时画出（相当于 `:focus-visible`）：鼠标点击已经表明落点，再画环只是噪音；按任意键后
环回来。

### 大文字与窄窗口（720 宽、115%）

界面文字大小只有 90 / 100 / 115 三档。旧的 200% 已取消：偏好文件里记着 200 时，启动、恢复与同步读取都落到 100%，写回的值
也是 100（`a_two_hundred_percent_preference_comes_back_as_one_hundred`）。窗口没有最小尺寸：默认 1120×720，用户可以
继续把它收窄，布局按内容换行而不是靠裁切。720 宽、115% 是收窄后的验收宽度——一行放不下"标题 + 说明 + 右对齐控件"。
设置页因此只有纵向滚动，横向必须靠换行容纳，不能靠裁切：被画到**卡片**右缘之外的行内控件既点不到，也没有横向
滚动条把它找回来。

- **设置行**（`view/settings.rs::settings_row`）：标题列最小 200（随缩放），控件列可以收缩到比控件自身
  宽度更窄（`min_w(0)`，不再是 `flex_none`）。一行放不下时整个控件换到标题下面一行，此时控件列占满行宽；
  行本身始终在卡片内，卡片始终在窗口内。
- **分段控件**（`components::segmented`）与**快捷键行**（`view/settings.rs::settings_keys_row`）：轨道可以窄于自身内容（`min_w(0)`），
  外面那层可收缩的列把它压到行宽，内部再按 `flex_wrap` 换行。单个分段（Shell 名称等）和单个 keycap
  保持完整不被截断，多出来的换到轨道内的下一行，所以"默认 Shell"这类名称很长的分段在更窄的窗口下会变成两行，
  而不是把轨道推出卡片；115% 下三个 Shell 名称在 720 宽的卡片里仍是同一行。
- **快捷键行**：名称列最小 180（随缩放），按键与三个操作按钮作为一组整体换行，因此长组合键（如
  `Ctrl+Alt+Shift+F12`）不会把按钮挤出卡片。
- **换行文本**：居中、有宽度上限、会换行的文字（空状态的标题与说明等）一律经 `components::capped_text(max_w, text)`
  放进列里，不在文字自身上同时写 `w_full()` 与 `max_w`。原因是布局引擎在 flex 列里求子项高度时用的是列的整宽，
  没有先套上 `max_w`；随后又按上限宽度布局和绘制。一句话在整宽下放得进一行、在上限宽度下要两行时，盒子只留了
  一行高，第二行就画到下面的按钮上（端口转发空状态的“使用。”）。`capped_text` 把文字放进一个 flex 行，行内的
  主轴尺寸会先按上限收窄再求高度，所以盒子高度就是实际画出的行数。新写的换行文字照此处理；不居中、不设上限的
  说明行（在 `flex_col` 里按默认拉伸占满宽度）不受影响。
  锁定测试：`an_empty_states_line_wraps_above_its_button_at_any_width`（直接按多种列宽与缩放布局 `empty_state`，
  断言说明盒子的高度不小于按其宽度换行后画出的高度、按钮在其下）与
  `the_forwards_and_host_list_empty_states_keep_their_button_below_the_words`（`forwards.empty`、
  `hosts.recent.empty`、`hosts.nomatch` 在 720 宽 115% 与 1120×720、中英文下标题、说明、按钮依次不重叠）。
  分区的加载中 / 失败 / 空卡片（`state_card_with_icon`：`section_card` 的空分支、`snippets.empty`、终端空页、远程桌面
  状态卡）同理：标题、说明与下一步各经 `capped_text`（上限 480）。原先是文字列自身 `w_full().max_w(480)`，说明在
  整卡宽下按一行求高、按 480 画两行，卡片因此矮了一行，下一步与说明压进卡片底部留白甚至越过卡片。锁定测试
  `a_state_cards_words_wrap_inside_their_own_boxes_at_any_width`（多种列宽、100% 与 115%：标题、说明、下一步依次
  不重叠，说明盒子不矮于画出的行，卡片底部留白完整、卡片在后续内容之前结束）。
- 新增设置行或行内控件时沿用这三条：控件外面不要再套 `flex_none` 容器；可能比行宽的内容自带 `flex_wrap` 与
  `min_w(0)`；控件仍按 `Metrics::hit_target()` 等因素绘制，不靠缩小命中区换取宽度。
- 该约束由三处测试锁定（`src/app/tests.rs`，均为 720 宽、115%、中文）：
  `every_settings_row_and_control_stays_inside_its_card_at_the_large_text`（每行、每个控件、以及分段控件里的
  每一个分段都在所属卡片内，卡片在窗口内）、
  `a_settings_control_wider_than_its_row_stays_inside_it_at_the_large_text`（比行宽的控件留在行与卡片内，
  每个分段仍在卡片内）、`the_shortcut_rows_keep_their_keys_and_buttons_inside_the_narrow_window`（长组合键下 keycap 与
  三个按钮都在卡片内）。测试读的是实测 bounds（卡片有 `settings.card.<id>` 选择器），不使用被裁切后的旧值；
  子项之间的切换由 `going_back_to_a_category_shows_the_item_that_was_open` 等测试读取页面身份
  （`settings_page_id`）断言，不依赖会残留上一帧的 debug bounds。

#### 走查时怎么确定窗口真的是 720 宽

720×480 是**逻辑**像素的参考窄窗口（不再是窗口下限）。Windows 在 150% 缩放下客户区要设成 1080×720 **物理**像素；
应用内 `window.viewport_size()` 报 `720x480`、`scale_factor()` 报 `1.5` 才算对上了这个宽度。非 DPI 感知的工具（未调用
`SetProcessDPIAware` 或等价 API）把 `1080×720` 当成逻辑像素，得到的是 1080×720 逻辑像素的窗口（物理 1620×1080），
比参考窗口宽 1.5 倍；再把它裁成 1095×729 的截图，右侧控件看起来就像"越过了窗口右缘"，其实只是截图被裁掉，
控件仍在卡片内。走查记录请同时写下 `viewport_size` / `scale_factor` 的证据（或使用 DPI 感知工具），不要只凭截图
判断右缘。

#### 2026-11-11 Windows 实窗（当时字号 200%，该档已取消）

这次进程在 `SHELLUNE_LOG_VIEWPORT=1` 时把下面两行原样写到 stdout。数字没有改过：

```
shellune-viewport viewport_size=1120x720 scale_factor=1.5 text_size=200%
shellune-viewport viewport_size=720x480 scale_factor=1.5 text_size=200%
```

界面文字从这一进程一开始就是 200%。第一行是窗口刚出现时的视口。客户区随后被收成 1080×720
物理像素（该窗口 dpi 144），进程改报 `viewport_size=720x480`、`scale_factor=1.5`。截这两张图时客户区仍是
1080×720 物理像素，日志没有再多一行。界面语言是英文。连的是已保存主机 lan109（192.168.0.109:22，root，受限 X11）。
应用里已有与 known_hosts 一致的 ED25519 指纹，这次没有再弹出确认框，也没有去连 212.60.153.19。

- `windows-720x480-200-x11-failure.png`：连上之后的终端。下一步整段是
  “Start an X server for this computer and make sure DISPLAY names it, or connect without X11 forwarding.”
  原因整段是 “no local X display is available: DISPLAY is not set”。头部第二行的 chip 是 X11 failed。
- `windows-720x480-200-transfer-failure.png`：同一扇窗口打开文件视图后的一次下载。远端路径是 `/root`（目录，不是缺失文件）。
  原因整段是 “Failed · the server refused the transfer: the source is a directory, not a file”。下一步是 Retry，整颗按钮都在客户区内。
  没有删除远端已有文件。文件视图打开时 X11 常显文本不画，chip 仍在；传输条可以滚动，原因和 Retry 同时留在客户区内。
  这张图里标题 “Transfers” 和数量徽章的顶部被当时的滚动口切平。标题和数量现在画在滚动区域之上，滚动口只裁下面的任务行。

同日更早一扇窗口留下的图不是这一进程：

- `windows-720x480-200-settings-theme.png`：主题分段 System、Light、Dark 都完整，说明是整句
  “Follow the system, or stay light or dark”。
- `windows-720x480-200-settings-shell.png`：默认 Shell 的三个名称都完整，Windows PowerShell、
  PowerShell 7、Command Prompt。
- `windows-720x480-200-settings-keys.png`：快捷键行里的 keycap、Change、Unbind 都完整。Go to hosts
  这一行整行都在窗口内。
- `windows-720x480-200-terminal-search.png`：搜索框是整句 “Search this terminal”，会话名
  Windows PowerShell 和 powershell.exe 都完整。
- `windows-720x480-200-tab-overflow.png`：全部标签对话框。副标题是整句
  “Every open tab, in strip order. Choosing one shows it.”
- `windows-720x480-200-shortcut-commands.png`：快捷命令对话框。副标题是整句
  “Insert types a command into the active terminal. Run also submits it, once.”
  New shortcut command 和 Close 都在。

macOS 没有实窗。Linux 上没有把本应用窗口打开在 192.168.0.109 的显示 :1 上。

标题换行、副标题换行、设置页换行和搜索框整句都没有改回去。`SHELLUNE_LOG_VIEWPORT` 默认仍关闭，只有这次进程设了 `=1`。

### 输入框（`text_field.rs`）

单行输入框支持：点击定位、拖动选择、双击选词、三击全选；Shift+方向键 / Home / End 扩展选区，
Ctrl+←/→（macOS ⌥）按词移动，Ctrl+Backspace / Delete 按词删除；Ctrl/⌘ + A / C / X / V 全选、复制、
剪切、粘贴（Windows 另有 Ctrl+Insert / Shift+Insert / Shift+Delete）；右键菜单给出同样四项。粘贴的换行
变为空格（快捷命令正文的字段是唯一例外：它保留换行并以 `\n` 画出，Shift+Enter 换行，见"快捷命令"）。
光标是绘制的细线，选区高亮（失焦时变淡），超长内容横向滚动并保持光标可见，获得焦点时显示
主色外环。`set_prominent(true)` 把一个输入框画成页面主入口（只用于首页快速连接栏）：高 40、圆角 12、
surface 底、静止 Raised，焦点环叠在 Raised 之上；尺寸不随窗口缩放（输入框统一按 `Metrics::default()` 绘制）。密码框可点眼睛图标临时显示，且拒绝复制 / 剪切；路径框（私钥、SSH 配置、自定义 Shell、上传
文件、下载目录）右端有"浏览…"按钮，调用系统文件选择框。私钥路径中的 `~` 展开为用户主目录。

### 快捷命令（`snippets.rs` + `view/shell.rs`）

一条快捷命令只有两样东西：名称和命令正文（`crates/core/src/snippet.rs` 的 `Snippet`，稳定 `SnippetId`）。
记录本身没有密码、口令、私钥或令牌字段，编辑器里明写"以明文保存在本机 `snippets.json`，不要放密码、私钥或令牌"；
手写的文件如果带上这些字段名，读取时会被拒绝并指出字段名，值不会被复制到任何提示里。

- 存储：`snippets.json`（`crates/storage/src/snippets.rs`），版本化、原子写入。文件不存在就是空库；文件损坏或来自更新的
  版本时**报错并保持原样**，库进入只读状态——`新建` 与 `保存命令` 变淡且不写入，对话框顶部说明读取失败、给出
  文件路径和"修复或删除后重新打开 Shellune"。因此空库永远不会覆盖别人的数据。
- 入口：命令面板的"快捷命令"组列出每条命令的两个动作——"填入「名称」"与"执行「名称」"，说明行给出正文首行和行数；
  同组还有"快捷命令…"（打开管理弹窗）与"新建快捷命令…"。两条动作都只作用于当前窗格：填入只写正文，执行才发送一次回车。
  没有活动终端时填入与执行都给出警告 toast（"请先打开终端"加下一步），不创建会话、不报告已执行；
  当前窗格的 Shell 已经结束时同样给出说明（"这条终端没有正在运行的 Shell"），一个字节都不写。
- 填入不追加回车：正文末尾的换行在写入前被去掉（`body_for_terminal`），所以一条保存时带结尾换行的命令不会因为"填入"
  而被执行；"执行"在去掉结尾换行后补且只补一个 `\r`，命令只进当前窗格一次，其它分屏与标签的输入不变。
- 操作层级：浏览（列表/面板检索）与编辑（编辑器）不改终端；"填入"是行里的普通动作（Secondary）且带提示
  "只把命令写进终端，不执行"；"执行"是安静的一级（Ghost）并带提示"写入命令后提交一次，提交前需要确认"。
  两条动作的文案都以"填入/执行"开头，任何地方都不会把其中一个说成另一个。
- 多行与高风险正文：填入与执行都走终端现有的粘贴安全路径（`TerminalPane::stage_snippet`），出现的是**窗口级模态弹窗**
  （见"多行粘贴确认"），带目标会话、行数、可滚动预览；执行时多一枚"确认后执行"chip，确认按钮写"执行这条命令"，
  填入时写"粘贴到这个会话"。取消、Esc 与关闭按钮都不写入任何字节。确认后仍然只写发起动作的那个窗格。
- 结果只报真实发生的事：`stage_snippet` 回答窗口"是否已写入"（写入 / 等待确认 / 正文为空）。只有真正写入时才发
  成功 toast（"已填入，未执行"/"已在终端执行一次"）；正文被暂存等待确认时只发一条中性提示（"已在粘贴弹窗中等待确认"，
  说明尚未写入、在弹窗里确认后才生效、选"不粘贴"或按 Esc 什么都不会写），取消后这条提示随弹窗一起消失，界面上
  不会留下任何"已填入/已执行"的说法。完成只由弹窗的确认产生。
- 管理弹窗（`Tool::Snippets`，`snippets.dialog`）：每条一行——名称、正文预览（换行显示为 `\n`）、多行 chip，
  右侧"填入/执行/编辑/删除"；删除先问一次（"删除这条快捷命令？"加名称，回答"删除"/"保留"），回答前不写文件。
  底部是"新建快捷命令"与"关闭"，下方常驻存储说明。空库显示空状态卡。名称与预览各占一行、放不下就用省略号截断，
  不换行：文字放大时四个动作标签比整行还宽，允许换行的正文会被压到几像素宽、一行一个字符，整行高过对话框本身
  （实测 2716 px 的行挤在 119 px 的正文区里）。所以名称/正文有一份最小宽度，动作按钮在这份宽度之外整体折到下一行，
  动作标签本身保持完整、不拆开。
- 编辑器：名称与命令两个字段，命令字段允许换行——`Shift+Enter` 换行、回车保存（`TextField::set_keeps_line_breaks`），
  换行在单行字段里画成 `\n` 而不是变成空格，保存的正文与输入的正文一致。长度超限时在输入过程中就给出提示，
  被拒绝的草稿留在屏幕上并显示原因。库不可写时保存按钮变淡，草稿仍可复制。
- 键盘：弹窗内 ↑↓ 移动行、Enter 填入当前行（安全的那条动作）、Esc 依次关闭编辑器→删除确认→弹窗；打开编辑器时
  名称字段直接获得焦点。
  指针目标沿用 `Metrics::hit_target()`，与其余对话框一致。720×480、115% 文字下弹窗本身与底部按钮留在窗口内，
  行内动作区在中间可滚动区域里，通过滚动到达，不隐藏任何动作；编辑器的名称/正文字段与保存按钮、删除确认的
  "删除"/"保留"也在同一块可滚动区域里，滚动即到达。

### 连接阶段与多行粘贴确认（R17）

两块浮层都用共享的 `components::modal`（居中、圆角 `radius::XLARGE`、`Elevation::Overlay`、`theme::scrim()` 遮罩），打开时接管键盘，关闭后把焦点
还给打开前的终端或字段。

**连接中**（`remote.dialog`）：标题是"连接到 <主机名>"，副标题是地址，右上角状态 chip 用 `remote_status_label` 的词与色调。
正在尝试时（`RemoteStatus::is_busy()`）正文顶部多一条阶段行：`Stage` 标签 + 状态点 + **真实状态词**
（未开始、正在连接、等待你的决定、已连接…），旁边在等待用户时加一枚"等待你操作"chip，下面一行写这次等待的对象
（服务器身份 / 代理命令 / X11 转发）；再下一行是等宽字体写出的目标地址。
阶段行只在尝试在飞或等待用户时出现：已失败的尝试由失败横幅说明，未开始的表单不假装有阶段。
底部两个出口：尝试在飞或有待答问题时是"取消连接"（结束这次尝试并关闭表单，不产生终端），有会话记录时是
"在后台继续"（表单收进该尝试的标签，点标签再回来）。按钮反映 `RemoteStatus`：进度文案不谎称仍在自动连接，
等待指纹/ProxyCommand/X11 时也不会继续显示"正在连接"。

**多行粘贴确认**（`terminal.paste`）：多行、带结尾换行或含控制字符的粘贴不再画成终端顶部的小卡片，而是窗口中央的模态
弹窗。标题"确认粘贴"，副标题是"<会话名> · <目标>"，正文依次是"目标会话"一行（等宽地址）、行数 chip（执行时再加一枚
"确认后执行"）、**完整正文**的可滚动预览（等宽字体、有自己的滚动位置，长块不被截断）、以及一行"按 Esc 或取消不会写入
任何内容"。底部是"不粘贴"与"粘贴到这个会话"（执行时"执行这条命令"）。

- 背景终端保留在遮罩后面但不能操作：遮罩挡住指针，窗口在弹窗打开期间安装一个 keystroke interceptor，把所有按键都
  当作这次作答处理——Esc 取消、Enter 确认、其余键什么也不做。因此 Ctrl+L、Ctrl+W 之类的绑定不会在弹窗后面生效，
  Shell 程序也收不到任何字节。
- 取消 / Esc / 关闭按钮都零写入，并清掉"等待确认"那条角落提示；确认只写发起这次粘贴的那个窗格（按 pane 的身份，
  不是"当前标签"），即使这期间切到了别的标签也一样。
- 作答后键盘回到原终端（`pane_focus_pending`），弹窗关闭，遮罩消失。
- 覆盖测试：`the_paste_confirmation_is_a_dialog_over_the_terminal_and_writes_nothing_until_answered`、
  `the_paste_dialog_writes_only_into_the_session_it_names`、
  `the_connection_dialog_names_the_target_and_stage_and_offers_cancel_and_background`、
  `the_tab_list_is_walked_with_the_keyboard_and_shows_the_shortcut_in_force`。

### 命令面板（`palette.rs` + `view/overlays.rs`）

参考 Tabby 的配置/连接选择器：一个快捷键打开、可输入过滤的统一入口。顶栏右侧的搜索按钮
与欢迎页的快捷键提示都指向它。未输入时按组列出：已打开的会话（带 Alt+N 提示与"当前"标记）、
可启动的 Shell、已保存主机（选中即打开连接表单）、快捷命令（插入 / 执行，见上）、五个分区、操作（关闭当前会话、新建主机、
导入 SSH 配置、切换语言、切换主题、文字大小、减少动态效果）。当前标签还会带四条标签操作：
重命名标签…、标签辨识色（当前 → 下一个）、恢复标签自动标题、不设标签辨识色，这样恢复自动标题与
清除颜色不需要指针（R2、R4）。输入后按匹配度平铺排序：标题前缀 > 词首 > 标题包含 >
说明包含 > 三个字母起的按序模糊匹配；每个词都必须命中。↑↓/Tab 移动、Enter 执行、Esc 或点击
遮罩关闭，关闭后键盘回到打开前的字段、终端或页面。无匹配时显示"没有匹配的结果"，Enter 不做任何事。
720×480、115% 文字下面板留在窗口内，列表本身滚动到最后一行（每条 72 px 高，一个屏只放得下两条），
搜索到某条快捷命令时"插入/执行"两行完整落在列表框内而不会被裁掉。
外观（R6，T8 / ADR-7 更新）：面板宽 560（`components::PALETTE_WIDTH`，窗口更窄时左右各留 24），画在与菜单、tooltip、
toast 相同的 `components::popover_surface` 上（raised + 强边 + 圆角 8 + Overlay），压在自己的遮罩上，进入时 Settle；
结果行一个大号控件高（36），左侧 24 的类型图块，有生效绑定的结果在行尾画键帽（`kbd`，读 `Keymap`，与菜单同源）；
底部按键提示条一个紧凑控件高（28）。面板和对话框一样记作"本帧画了对话框"，关闭后遮罩由窗口用 `modal_scrim_exit` 淡出（减少动效时随面板立即消失），
由 `the_palette_settles_like_a_dialog_and_its_dimming_fades_on_close` 锁定。


终端内 Esc、Ctrl+C、Ctrl+L、Alt+C 与 Alt+R 属于终端程序；例外只有终端搜索条持有键盘时，Esc 关闭搜索条并交还键盘、Alt+C / Alt+R 切换条上的两个开关（见上）。语言在设置页切换（Ctrl+, 在终端内也有效，除非该绑定被解除）。

命令面板、标签菜单和会话菜单记下打开前的焦点，关闭后还回去。点开这些层时，点击可能先把焦点移到页面上；上一帧正在编辑的字段或终端不会被这次页面焦点盖掉。主机关闭或保存后，下一帧回到打开对话框之前的位置。

减少动态效果有两个来源：应用内设置（设置页“动态效果”、命令面板“减少动态效果”，默认是完整）与系统设置（见 2.3 第 5 条）；任一方要求减少即生效。加载状态的圆点会做一次约 200ms 的淡入；减少时圆点直接以不透明画出，标题、原因和下一步仍在。GPUI 0.2.2 不报告系统设置，由应用自己读取，因此跟随系统。没有 AccessKit，屏幕阅读器兼容未验证，见 `04-cross-feature-walkthrough.md`。

## 5. 组件清单（`components.rs`）

组件只画外观，交互（点击、焦点、动作）由调用方加。每个控件的**外观只有一张表**：`ControlFace`
给出某个 [`Variant` / 字段 / 行 / 菜单项] 在某个 `ControlState` 下的填充、边框、边框宽度、文字色、
前导标记与"是否等待中"，值全部是 `theme::Token`。组件自己只画"静止"那一行，并把"悬停"那一行注册
成指针样式；需要焦点、按下、选中、不可用或进行中的调用方，用它自己的状态调用同名组件
（`button_with_state`、`row_with_state`、`field_frame`），而不是另写一组颜色。新写的页面不得手写勾选框、
对话框外壳或菜单卡片。

**迁移进度（地基任务之后）。** 已迁到组件：三处勾选框、`view/shell.rs` 的对话框外壳、会话行菜单与 toast。
已无手写的勾选框、对话框外壳或菜单卡片。

其余界面任务（T7）完成的迁移：命令面板外壳改用 `dialog_surface`（与 `modal` 同一个表面）并进入 Settle、关闭后遮罩
淡出；主机菜单、标签菜单、`terminal.menu`、`files.menu`、`shell.picker` 与输入框的 `field.menu`（`text_field.rs`，
剪切 / 复制 / 粘贴 / 全选）改用 `menu_card` / `menu_entry` /
`menu_divider` / `menu_section` / `menu_shortcut`，进入用 Rise；分组对话框底部改用 `modal_footer`，"删除分组"为 Danger；
文件列表的标记方框与 SCP 的"递归复制"改用共享勾选框；端口转发的空状态改用 `empty_state`，规则行与编辑表单改用 `card`；
远程桌面的控件条、状态卡片、提示条与证书卡片见 1.4；串口条的回显 / 换行 / 刷新改为紧凑按钮，分隔用 `divider_vertical`。
文件视图的行内问题有意留在栏内（理由见"文件视图"一节）。菜单截图场景：`menu-tab`（断开会话的标签菜单，
含"重新连接"）、`menu-host`、`menu-files`、`menu-terminal`、`menu-shell`。

| 组件 | 规范 |
| --- | --- |
| `button` / `button_sized` / `button_with_state` | 五种变体（v2）：Primary（实心 accent + Raised）、Secondary（面板底 + 强边）、Ghost（无底，悬停出底）、Danger（danger tint）、Icon（方形仅图标，外观同 Ghost，必须带 tooltip）；`ButtonSize` 紧凑 28 / 常规 32 / 大号 36，圆角 6，MEDIUM 字重，标签 13（紧凑为 12）。旧的 Soft 已删除，原用处改为 Secondary |
| `button_face(variant, state)` | 五种变体 × 八态外观表（测试逐格校验文字与边的对比度：有不透明底的读自己的底，底是遮罩的读它叠在五个层上的结果，无底的读五个层与叠在各层上的选中底，两套主题；同一变体的八态互不相同） |
| `icon_label_button` / `compact_button` / `icon_only_button` / `danger_icon_button` / `primary_icon_button` / `icon_button` | 带图标按钮、紧凑按钮（`ButtonSize::Compact`，28）、方形图标按钮（`Variant::Icon`，≥ 24 指针目标）；`primary_icon_button` 是去掉文字的主动作（主按钮的 accent 底与字色，窄横幅的“重新连接”，T9） |
| `field_face` + `field_frame` | 输入框：surface 底、弱边，焦点 2px 焦点环，校验失败 2px 危险边 |
| `segmented` + `segment` | 互斥选择：凹槽 + 选中滑块（滑块 Raised） |
| `switch(on)` | 设置开关：34×20 胶囊轨道，圆钮在左（关，`TRACK_OFF`）或右（开，`TRACK_ON`），悬停用对应 `*_HOVER`，圆钮 Raised；状态先由位置表达，圆钮在每种轨道上 ≥ 3:1 |
| `checkbox(id, label, checked)` / `checkbox_box(checked)` | 16px 方框（圆角 4）：未选为 surface + 强边，悬停（`CHECK_GROUP`）变 accent 边；选中为 accent 填充 + 勾 |
| `card` | 内容分组：surface + 弱边 + 圆角 8 + Raised，内边距 16（新界面少用卡片，优先 `list_row` 与 `toolbar`） |
| `status_card(tone, emblem, title, detail)` + `status_card_slot` | 叠在终端上的会话状态卡：raised + 强边 + 圆角 12 + Overlay，色调徽记块 36 + 标题 + 一行说明；吃掉指针，动作由调用方追加，进入时 Rise。卡片本身不带宽度，一律经 `status_card_slot` 放置（`capped` 规则，上限 `STATUS_CARD_WIDTH` 440），按画出的宽度求高度——修复了“展开诊断”时文字溢出卡片底边（测试 `a_status_card_is_as_tall_as_the_diagnostic_it_paints`） |
| `search_field(field, clear, shortcut, state)` | 唯一的搜索框：放大镜图标 + 调用方的文本框 + 可选清除按钮 + 可选快捷键提示（`kbd`）；外观取 `field_face(state)`，高 32、圆角 6。文本框的占位文字比框宽时以“…”结尾（`text_field.rs` 绘制时截断，测试 `a_placeholder_wider_than_its_field_ends_in_an_ellipsis`）；已输入的文字仍横向滚动 |
| `list_row(ListRow, state)` / `list_row_frame(state)` / `list_row_face` / `leading_icon` | 列表行：扁平（静止无底无边），悬停 hover 底、选中 selected 底 + 左侧 2px accent 竖条（画在内边距里，内容不移动）、焦点 2px 焦点环；左侧头像或图标块（32），两行文字（名称 13 MEDIUM、副行 12，放不下时以画出的“…”结尾，测试 `cut_words_in_rows_banners_and_sidebars_end_in_a_drawn_ellipsis` 读回排版后的行来校验），右侧元信息（12、muted）与动作区；`reveal_actions` 时动作只在悬停（`ROW_GROUP`）或选中时显示。端口转发列表已改用它 |
| `compact_row_frame(state, marked)` | 紧凑列表行（T7）：与 `list_row_frame` 同一张面表、同一个 `ROW_GROUP`，最小高一个紧凑控件（28）、无上下内边距、间距 8；`marked` 为批量标记行（`ACCENT_TINT` 底，悬停不覆盖）。文件列表与“上级目录”行用它 |
| `inline_form(tone)` + `form_heading(tone, icon, title)` + `form_label(label)` | 栏内 / 对话框内的问题与短表单（T7）：raised 底 + 强发丝边（危险、警告为该色调的边）+ 圆角 8 + Raised，内边距 12、间距 8；头部是指针目标大小的色调图块 + 一句（body、500）；字段标签 caption、500、次要色；主次按钮放在最后一行、占满宽度并靠右，主按钮在最右、取消在它左边，不用大面积强调色底。文件栏问题、传输覆盖确认、隧道表单与停止 / 删除 / 暴露确认用它，取代这些地方的 `banner` 与 `card` |
| `crumb(current)` + `crumb_separator` | 面包屑的一段与段间箭头（T7）：次要文字、圆角 4、悬停浅底与主文字色，当前段主文字 + 500；箭头 caption 大小、muted |
| `column_divider` / `HAIRLINE_WIDTH` | 两栏之间从顶到底的发丝线；发丝线宽度常量（`divider_vertical` 与文件栏宽度计算也用它） |
| `sidebar_item(icon, label, count, selected)` / `sidebar_icon_item` | 侧栏项：高 32、圆角 6，图标 16 + 文字（省略号）+ 右对齐计数（12、muted）；选中为 selected 底 + 主色 + MEDIUM；折叠侧栏用仅图标的方形项 |
| `toolbar` | 页面或栏的头部工具条：一行、居中对齐、间距 8、最小高 44、底部发丝线；调用方放标题、搜索框和命令，`flex_1` 的子元素占剩余空间 |
| `inline_banner(tone, icon, message)` | 内容上方的状态横幅（ADR-6 用）：色调 tint 底 + 底部色调发丝线，最小高 36；色调图标 + 一句话（省略号截断）+ 调用方追加的紧凑按钮与关闭按钮。不遮挡下方内容 |
| `progress_bar(id, Progress)` | 细进度条（高 4、全圆角）：确定型按比例填充；不确定型一段 30% 宽的填充以 `PROGRESS_SWEEP` 1200ms 循环扫过，减少动效时静止居中（`progress.determinate` / `.indeterminate` / `.still`） |
| `progress_line(id, from, to, duration, still, metrics)` | 启动页的确定型细进度条（2px 高、全圆角、accent 填充、`LOADING_TRACK` 轨道，`progress.line`）：填充从 `from` 以 ease-out-cubic 缓动到 `to`（新的 `id` 重播，同一 `id` 画在 `to`）；减少动效或 `still` 时直接画 `to`。启动页用它追阶段目标（第 1.5 节） |
| `skeleton_text(fraction)` / `skeleton_row(id)` | 骨架屏：文字行（高 10、圆角 4、selected 底）与列表行（头像块 + 两行）；允许动效时缓慢呼吸，减少动效时静止 |
| `kbd(label)` | 键帽：高 `KBD_HEIGHT`（18）、圆角 4、面板底 + 强边（底边 2px），字号 caption − 1；取代旧 `keycap` |
| `checkbox_target(checked)` / `labelled_switch(on, word, selector)` | 指针大小的勾选目标（首页多选列）；带状态词的开关（设置页），取代两处本地实现 |
| `row` / `row_with_state` + `stacked` | 列表行：surface 底，悬停、选中（底色 + 前导竖条）、焦点、失败、进行中 |
| `popover_surface()` | 浮层表面（T8 / ADR-7）：raised + 强边 + 圆角 8 + Overlay。菜单卡、浮动控件条、命令面板、tooltip 与 toast 共用，测试 `menus_tooltips_toasts_and_the_palette_share_one_floating_surface` 逐个读回样式 |
| `menu_card` + `menu_entry` + `menu_divider` | 右键/下拉菜单：`popover_surface`，最小宽 `MENU_MIN_WIDTH`（200），内边距 4；条目一个紧凑控件高（28，T8 由 30 改）、圆角 4（与卡片内边距同心），标签占满剩余宽度，禁用项保留位置，危险项悬停为 danger tint；`menu_item`（取值菜单）同为 28 高 |
| `menu_shortcut` / `menu_section` / `menu_enter` | 条目尾部的当前生效快捷键（caption、muted，右对齐）；菜单内分组小标题；菜单进入动画（Rise，减少动效时直接画出） |
| `floating_bar` | 浮在画面或页面上的控件条：raised + 强边 + 圆角 8 + Overlay，吃掉指针；远程桌面的缩放与全屏 |
| `dialog_surface` | 对话框级表面：overlay + 强边 + 圆角 12 + Overlay，裁切到圆角；`modal` 的卡片（T8 起命令面板改用 `popover_surface`） |
| `menu_panel` + `menu_item` + `menu_item_face` | 取值菜单（选中项保留选中外观） |
| `modal(viewport, id, width, ModalContent)` / `modal_with(..., ModalBehaviour)` | 对话框外壳（尚未迁移的见上）：scrim 遮罩（按下不关闭；`ModalBehaviour::dismiss` 仅给无内容可丢的对话框），overlay + 强边 + 圆角 12 + Overlay，头部固定、主体滚动（`{id}.body`，可交给调用方的 `ScrollHandle`）、底部固定，遮罩为 `{id}.backdrop`，进入时 Settle；关闭后遮罩由窗口用 `modal_scrim_exit` 淡出 |
| `modal_footer` / `modal_icon(icon, tone)` | 对话框底部动作行（顶部分隔线、右对齐、可换行，主操作最后）；头部图标块（默认 accent tint，危险问题用色调 tint） |
| `modal_footer_noted(note, actions)` | 带说明的底部动作行（T8）：左侧安静说明（caption、muted，可换行，最小宽 `FOOTER_NOTE_MIN_WIDTH` 160），右侧动作组 `ml_auto` 靠右、可换行；放不下时动作组整体换到下一行仍贴右缘，主操作始终在最右。批量任务的四个对话框用它（原来换行后动作贴左） |
| `form_row` + `form_field(label, control, share, min)` / `form_cell(share, min)` | 对话框的两列自适应表单（T8 / ADR-7）：一行两个（或多个）字段，每格从 0 起按 `share` 比例分剩余宽度、保底 `FORM_FIELD_MIN_WIDTH`（200，短字段如端口 `FORM_SHORT_FIELD_MIN_WIDTH` 96），底线放不下时整格换行，所以宽窗口两列、窗口窄到 440（115%）时一列；标签是 `form_label`。主机表单（地址 / 端口、名称 / 分组）、远程桌面 / 串口表单的成对字段、批量任务的“同时运行 / 任务名称”、快捷命令编辑器用它。测试 `the_host_form_is_two_columns_wide_and_one_column_narrow` |
| `form_section(title)` / `form_hint(text)` / `form_rule()` | 表单分组标题（body、600、主色）、字段下说明（caption、muted）、分组间发丝线 |
| `disclosure(open, title, hint)` | 表单的折叠段头（“高级”，T8）：箭头 + 名称 + 一行内容说明（省略号），扁平、弱边、悬停出底，调用方加 id、点击与“已使用”chip；主机表单的高级设置默认折叠 |
| `nav_item(icon, label, selected, ringed)` / `accent_mark(shown)` | 页面自带的导航列（设置，T8）：分类项一个控件高、带图标，子项无图标、缩进、一个紧凑控件高；选中 = selected 底 + 2px accent 竖条 + MEDIUM，键盘所在项另画焦点环（常驻透明边，不挪字；设置导航仅在导航列持有键盘时传 `ringed`）；`accent_mark` 是那条竖条本身（不选时为同宽透明占位），设置卡片行首也用它 |
| `dialog_face` / `dialog_panel` / `dialog_header` | 对话框表面令牌与内嵌标题块 |
| `toast(tone, icon, title, detail)` | 通知：`popover_surface`，宽 `TOAST_WIDTH`（340），色调图标圆底（`ICON_TILE` 24，按语义使用 `Check` / `AlertTriangle` / `AlertCircle` / `Info`）；`toast_stack` 底部抬高至会话底栏之上（`SHELL_BAR_HEIGHT + spacing::SNUG` 净空）避免遮挡右下角 `terminal.search` / `session.more`，关闭按钮与位置由调用方加，进入时 Fade |
| `empty_state(icon, title, body, action)` | 空状态：accent tint 图标块（圆角 12）+ 标题 + 可选说明（可换任意行）+ 可选动作；标题与说明经 `capped_text` 居中、上限 420，动作总在文字下方（`empty.state.title` / `.body` / `.action`）；不带状态色调；调用方在外层加自己的选择器 |
| `capped_text(max_w, text)` | 居中、有宽度上限的换行文字：按上限宽度求高度，盒子与画出的行数一致（见 4“换行文本”） |
| `enter_in_turn(element, id, turn)` + `turn_timing` / `turn_curve` | 一组物体依次淡入：第 `turn` 个晚 `TURN_DELAY`×turn（到 `LAST_TURN` 为止），淡入 BASE，只动透明度；减少动效时直接画终态 |
| `banner` / `notice` | 提示、失败、确认问题（色调底 + 左侧 3px 色条）；完成后的通知 |
| `state_card` / `state_card_with_icon` | 加载中 / 失败 / 空：surface 卡（圆角 8）+ Raised，色调 tint 徽记块；标题、说明、下一步经 `capped_text` 居中、上限 480（`state.card.title` / `.body` / `.step`） |
| `chip` / `tag` / `badge` / `kind_badge` / `status_dot` / `status_mark` | 状态（形状 + 词 + 色调）、分组、数量、类型、状态标记；chip、tag、badge 为圆角 4 的小块（不再是胶囊）。`Tone::Info` 新增，标记为“圆环加中心点”（`StatusMark::Target`） |
| `capped(max_w, block)` | `capped_text` 的通用版：任意块居中、有宽度上限，并按画出的宽度布局（远程桌面状态卡、`status_card_slot` 使用） |
| `pulsing_status_dot` | 带名字的状态点；进行中（如连接中）且允许动效时以 `Motion::PULSE` 脉动，减少动效时静止（标签用） |
| `endpoint` / `copy_on_click` | 等宽端点值，点击复制 |
| `loading_indicator` | 进行中指示条（BASE 淡入）；减少动效时静止画出 |
| `section_label` / `field_row` / `divider_vertical` | 分组标题、表单标签行、竖线分隔 |
| `Tooltip` / `tooltip()` | `popover_surface`（T8 起圆角 8，与菜单、toast 一致），内边距 8 / 4，caption 字号，随窗口缩放（`theme::metrics()`） |
| `icons::icon(Icon, size, color)` | 内嵌线条图标 |

新增分区必须复用同一组组件、`ControlState` 与 `SectionState`，不为某个分区单独发明状态表达、私有
配色或阴影。

**组件清单页（`src/app/catalog.rs`）。** `catalog::design_catalog` 在一页四栏里画出全部共享组件的各种状态（按钮五变体 × 静止 / 悬停 /
焦点 / 禁用与三种尺寸、输入框三态与搜索框、分段 / 开关 / 勾选、全部色调的 chip 与标记、标签 / 计数 / 类型 / 键帽、三种横幅、
确定与不确定进度条、骨架行、侧栏项、三种状态的列表行与工具条、菜单、toast、状态卡、颜色色板与字阶）。截图场景
`design-catalog`（分类 `design`，深浅各一张）是视觉回归基准；测试 `every_section_of_the_catalogue_stays_inside_its_column`
在 1120×720 的深浅两套与 115% 下检查每一节都在自己的栏内，并在 100% 下检查四栏的底边都在 720 以内（整页一屏拍全，
不靠滚动）。四栏依次是：按钮 + 标签；输入框 + 反馈；列表 + 令牌（字阶排成一行换行）；浮层。列表行的头像字取
`host_initials`（build-01 → B1、api-02 → A2、edge-03 → E3），与首页一致。

**v2 替换的本地复制品**：`view/panes.rs` 的 `components_banner` 包装 → 直接用 `components::banner`；`view/forwarding.rs` 的
`components_row`（卡片当行用）→ `list_row_frame` + `leading_icon`；`view/home.rs` 的本地 `checkbox` → `checkbox_target`；
`view/desktop.rs` 状态卡的手写居中限宽 → `capped`；`view/shell.rs` 的 `switch_control` 手写开关行 → `labelled_switch`；
会话状态卡的限宽 → `status_card_slot`。`view/shell.rs::modal` 只是把 `components::modal` 放进覆盖层并记下遮罩淡出，保留。
T7 又把文件视图、传输、端口转发与远程桌面里的手写外形换成组件：文件栏头的自绘图块与标题行 → `toolbar` +
`leading_icon`；面包屑的带框行与自绘段 → `crumb` / `crumb_separator`；文件行与上级行的自绘悬停 / 选中 →
`compact_row_frame`；传输进度条的自绘槽与填充 → `progress_bar`；文件问题、传输覆盖问题（3px 左边的手写块）、隧道表单与确认 →
`inline_form`；桌面的 `state_card_with_icon` 与证书 `card` → `status_card` + `status_card_slot`；两处 `w(px(1))` 竖线 →
`column_divider`。

**T8（ADR-7）：设置、对话框、菜单、命令面板、tooltip、toast 与批量任务。** 行为与选择器不变，只换外观；键盘路径仅对话框 Tab 顺序随分组调整（见下）：

- **设置**：导航列改为 `nav_item`（见 1.0 “设置页”与上表），分类标题 20，卡片行首的选中竖条与自带页的小标题竖条改为
  `accent_mark`。截图工具打开某个设置页时同样滚到该分类（`capture_settings_page` 调 `settings_reveal`；此前
  `settings-keys*` 截图拍到的是页面顶部的外观分类）。
- **对话框**：三段式不变（固定头部 → 滚动正文 → 固定底部，主按钮最右、取消在其左）。头部标题从 20 降到对话框标题字号
  16（`title_size`，ADR-1），头部上内边距 16。表单用 `form_row` / `form_field` 两列自适应，组标题与分隔线用
  `form_section` / `form_rule`，“高级”用 `disclosure` 且默认折叠。表单正文行距 12。批量任务库的底部改为“关闭 → 新建任务”，
  主按钮回到最右。
- **主机与连接对话框分组与 Tab 顺序**：SSH 主机对话框按 `form_group` 分为“基本信息”（地址、端口、用户，下一行名称、分组）、
  “登录方式”（密钥文件、密码/口令）与折叠的“高级”各组；RDP/VNC/串口连接对话框分为“基本信息”与“选项”两组。Tab 顺序随分组
  调整为与绘制顺序一致：SSH 为地址 → 端口 → 用户 → 名称 → 分组 → 密钥文件 → 密码 → （展开高级时）代理 → 启动命令 → 保活三项；
  连接表单先走“基本信息”组的输入框（如 RDP 地址 → 端口 → 用户 → 域 → 名称 → 分组），再走“选项”组（RDP 共享文件夹、串口数据位），
  归组由 `ConnectionFormField::is_basic` 决定；`ssh_host_dialog_tab_follows_the_drawn_order`、`rdp_` / `vnc_` / `serial_connection_dialog_tab_follows_the_drawn_order`
  按输入框 bounds 的 y/x 排序后与 Tab / Shift+Tab 实际走过的顺序比较。登录方式分段控件不在文字框 Tab 环内（与 T8 前相同）。
  除此之外，T8 只改了一处键盘相关的显示：设置导航的焦点环（见 1.0）。
- **菜单**：`menu_entry` 与 `menu_item` 28 高（`a_menu_entry_is_one_compact_control_high`），菜单卡、跳板选择器的
  候选行同为 28；菜单卡与命令面板、tooltip、toast、远程桌面浮动条共用 `popover_surface`。
- **命令面板**：宽 560、raised 浮层 + 圆角 8，结果 36 高并带生效键帽（`the_palette_is_560_wide_and_its_results_name_their_keys`）。
- **批量任务**：库、历史、编辑与运行四个对话框的底部改为 `modal_footer_noted`（`the_batch_actions_stay_on_the_right_of_their_footer`）；
  运行页与历史详情的主机行、库与历史的条目行从“面板底卡片行”改为扁平 `list_row_frame`（悬停出底，选中为 selected 底 +
  accent 竖条）；编辑页的“同时运行的主机数 / 任务名称”改为一行两列的 `form_field`。


组件的测试在 `components.rs` 与 `theme.rs` 各自的 `#[cfg(test)]` 模块里：八种状态逐格校验文字对比
度（禁用态取 3:1 图形下限）、同一变体的八种状态互不相同、焦点与校验失败只加宽边框、浮层使用 overlay /
raised 与强边、开关与勾选框的轨道与勾可读、每个共享组件在每种状态下都能构造；`theme.rs` 校验对比度、
层级单调、动效时长与减少动效。

## 6. 文案

- 产品名与两句简介由 `product.rs` 提供；简介按界面语言显示在空的本地终端页。
- 其余文案全部在 `copy.rs` 的双语表中；英文计数用 `copy::counted` 处理单复数。

## 7. 2026-11 全应用重做的交付汇总（T9，ADR-9）

本节只做索引与汇总：规则本身写在各自的章节里，这里给出位置、最终数字与仍未关闭的事项。各节与代码不一致时以代码为准。
本节是 2026-11 T9 的记录，其中 200% 是当时仍在的字号档；该档已取消（见「大文字与窄窗口（720 宽、115%）」），
所以下面提到的 200% 截图、实测与待办都属于历史，不再重拍。

### 7.1 交付项与位置

| 交付项 | 位置 | 说明 |
| --- | --- | --- |
| 设计 token 表（R1，ADR-1） | §2.1 颜色、§2.2 层级、§2.3 动效、§2.4 形状间距与字 | 深浅两张独立的表；交互填充为半透明叠加（§2.1）；偏离 ADR-1 的值在 §2.1 写明 |
| 组件规范（R1，ADR-2） | §5 组件清单 | 按钮五变体与 28 / 32 / 36 高、`search_field`、`list_row`、`sidebar_item`、`toolbar`、`inline_banner`、`progress_bar` / `progress_line`、`skeleton`、`kbd` 等；`design-catalog` 截图逐个画出 |
| 首页入口新旧对照表（R2，ADR-3） | §1 “首页入口新旧对照”（T4） | 15 个入口；分组的键盘与 `⋯` 菜单路径为 T4 返修后增加 |
| 启动阶段与实测耗时（R3，ADR-4） | §1.5 | 四阶段、进度条、失败停下、交接动效、减少动效；本机实测见 7.4 |
| 终端选择规则（R4，ADR-5） | §2 “终端选择” | 半格、阈值、双击 / 三击、Shift、Alt 块选、窗口级捕获、边缘自动滚动；截图 `terminal-selection*` |
| 终端状态横幅规则（R5，ADR-6） | §1.0 “终端状态横幅”、§1.4（远程桌面同一横幅） | 一行横幅 + 可展开详情（最高 160 且不超过窗格 40%）+ 复制诊断；Tab 可达、Esc 关闭，开始重连时关闭记录清除；窄横幅的重连为 accent 图标；重连中为不定进度 + 取消 |
| 发布流程原生构建（R6，ADR-8） | `packaging/README.md`、`vendor/README.md`（T2） | 运行链接见 7.5 |
| 按钮顺序规则（ADR-7） | §“文件视图（SFTP）”中的“按钮顺序（全应用，T7 起）” | 一组回答按钮靠右，主按钮（Primary / Danger）在最右，取消 / 拒绝 / 保留在它左边；Tab 顺序与视觉一致；T8 对话框三段式沿用 |
| 选择器变更表（ADR-9） | 7.2 | 汇总 T1–T9 的删除、改名与新增 |
| 守卫最终基线（ADR-9） | §2.5、7.3 | `scale` 13 / `px` 2（迁移前 280 / 91） |
| 截图（R7，ADR-9） | 7.4 | 全部场景重拍，深浅、中英、200% |

### 7.2 选择器变更表（T1–T9）

测试读的是 `debug_selector`。下表列出这次重做删除、改名与新增的选择器；没有列出的选择器名字与含义不变。
`<n>` 是行号，`{id}` 是会话或窗格编号。各任务的明细在对应章节（T4 见 §1、T6 见 §1.0、T7 见“文件、传输与转发的任务连续性”节末）。

| 界面 | 删除 | 改名（旧 → 新） | 新增 | 任务 |
| --- | --- | --- | --- | --- |
| 组件库 | — | — | `catalog`、`catalog.column.<n>`、`catalog.section.<标题>`、`catalog.type.<字阶>`（设计目录）；`list.row.title`、`progress.determinate`、`progress.indeterminate`、`progress.still`、`progress.line`、`skeleton.row` | T3、T5 |
| 首页 | `home.quick`、`home.quick.results`、`home.quick.result.<n>`、`home.quick.empty`、`home.dashboard`、`home.cards`、`home.all`、`home.recent*`、`home.favorite*`、`home.group.<n>`、`welcome.quick`、`hosts.view.groups`、`hosts.count`、`hosts.surface`、`hosts.statuses`、`hosts.tree.all`、`hosts.group.chooser`、`hosts.group.option.<名>`、`hosts.group.actions`、`hosts.group.<n>`（列表分组头）与其 `.toggle`、`hosts.filter.favorites(.chip)`、`hosts.filter.group`、`hosts.forwards.<n>` | `hosts.group.tag.<n>` → `hosts.tag.<n>`；`hosts.connection.open.<n>` → `hosts.connection.state.<n>` | `home.sidebar`、`home.sidebar.folded`、`home.sidebar.toggle`、`home.sidebar.groups`、`hosts.view.favorites`、`hosts.favorites.all`、`hosts.filter`、`hosts.filter.menu`、`hosts.filter.status`、`hosts.filter.sort`、`hosts.sort.*`、`hosts.more`、`hosts.more.menu`、`hosts.more.group.rename`、`hosts.more.group.delete`、`hosts.tree.<n>.more`、`hosts.group.menu`、`hosts.direct`、`hosts.nomatch.connect`、`hosts.empty.local`、`hosts.favorites.empty`、`hosts.group.empty`、`hosts.state.<n>`、`startup.page.sidebar`、`startup.page.list` | T4、T5 |
| 首页（位置移动，名字不变） | — | — | `hosts.import`、`hosts.batch.open.list` 从工具栏按钮移进 `⋯` 菜单；`hosts.group.rename.open` / `hosts.group.delete.open` 从列表上方按钮移进分组菜单；`home.local` 从标题行移到侧栏底部 | T4 |
| 启动 | `startup.splash.tagline` | — | `startup.splash.mark`、`startup.progress`、`startup.stage`、`startup.failure`、`startup.failure.title`、`startup.failure.reason`、`startup.failure.hint`、`startup.continue`、`startup.open_folder` | T5 |
| 终端横幅（T9） | — | — | `terminal.failure.room`（展开详情的外层，按窗格高度 40% 限高；`terminal.failure.detail` 仍是滚动的那一层） | T9 |
| 启动失败（T9） | — | 两个按钮从 `startup.failure` 面板内移到其下方的 `startup.failure.actions` 行（`startup.continue` / `startup.open_folder` 名字不变） | `startup.failure.actions` | T9 |
| 标签（T9） | — | 紧凑形态下 `session.tab.close.<n>` 从行内移到标签右端的浮层，悬停才显示（名字不变） | — | T9 |
| 截图场景（T9） | — | — | `forwards-host-picker`、`terminal-selection`、`terminal-selection-block` | T9 |
| 终端横幅与底栏 | `terminal.status.overlay`、`terminal.status.scroll`；底栏常驻工具 `terminal.grid.size`、`terminal.mode`、`terminal.copy`、`terminal.font.decrease`、`terminal.font.increase`、`terminal.font.reset` | 同名动作移进菜单：`terminal.more.size`、`terminal.more.mode`、`terminal.more.copy`、`terminal.more.font.decrease/.increase/.reset` | `terminal.banner`（仅重连中的横幅行）、`terminal.failure.copy`、`terminal.failure.detail`、`terminal.failure.dismiss`、`terminal.reconnecting`、`terminal.reconnecting.headline`、`terminal.reconnecting.cancel`、`terminal.reconnecting.dismiss`、`session.more.scroll`；保留 `terminal.failure(.headline/.next/.details/.diagnostic)` | T6 |
| 分屏窗格 | — | — | （T9）窗格头 `pane.active.mark` 改用 `accent_mark`，名字不变 | T9 |
| 文件、传输、远程桌面 | 传输对话框无任务时的 `state.card` | `state.card` → `transfers.empty`（同一位置） | `files.toolbar`、`files(.local).loading.skeleton.<n>`、`transfers.progress.bar.<n>`、`desktop.status.headline`、`desktop.status.detail`、`desktop.certificate.fingerprint`；远程桌面结束时的 `terminal.failure*` 从卡片变成横幅（同名） | T7 |
| 端口转发 | 隧道表单里主机分段控件的各段 `forwards.host.<n>` | `forwards.host` 名字不变，从分段控件变成字段式选择器 | `forwards.host.picker`、`forwards.host.search`、`forwards.host.options`、`forwards.host.option.<n>`、`forwards.host.name`、`forwards.host.none` | T7 |
| 对话框与设置 | — | — | 主机表单分组 `hosts.group.basic/.auth/.route/.startup/.keepalive/.transfer/.x11`；连接表单分组 `connection.group.basic/.options` | T8 |
| 终端选择 | — | — | 无新选择器（选择是网格内的状态，测试直接读 `TerminalPane` 的选区） | T1 |

### 7.3 守卫最终基线

§2.5 的表是逐任务的记录。最终：`scale(<数字>)` **13**、`px(<非零数字>)` **2**（`theme.rs` 的 `SCALE_BASELINE` / `PX_BASELINE`，
`bare_lengths_only_ever_go_down` 断言）。迁移前（`d59a63f`，同一口径）为 280 / 91，ADR-9 要求降到一半以下（≤ 140 / ≤ 45），
最终为 4.6% / 2.2%。余下的 13 个 `scale(<数字>)` 是 `Metrics` 度量本身的定义，2 个 `px(<数字>)` 是 `STRIP_ROUNDING_SLACK`
与标签条滚动步长的 1px 下限。颜色字面量只在 `theme.rs` 与 `capture.rs`（`views_name_tokens_instead_of_colour_literals`）。

### 7.4 截图与实测

- **终版全量重拍（T9-A2，Owner）**：A1–A9 和启动失败页修复都合入后（6b2a527 加上 `tab_title` 文档注释修正），重新全量运行
  `shellune --ui-capture --out target/t9/final2`：92 个场景、466 张全部拍成，`status` ok，`failures` 为空，用时约 24 分钟。
  以这一批作为交付截图。下面两条是 A1–A9 修复之前的那次全量运行和补拍，留作对照。
- **全部场景重拍（T9，A1–A9 修复前）**：`shellune --ui-capture` 全量运行（Windows、150% DPI 的隐藏窗口，2026-11-19）：
  91 个场景、计划 461 张、拍成 461 张，`failures` 为空，用时约 23 分钟；其中深色 311、浅色 150、中文 115、200% 文字 100。每个场景至少有深色、浅色、中文与 200% 文字各一张：
  场景没有声明中文或 200% 变体时，`Scenario::shots` 补一张浅色中文与一张深色 200%（设计窗口 1120×720），
  `a_full_run_shoots_every_scenario_in_both_palettes` 断言每个场景都有这四类且文件名不重复。原有场景的 720×480、115%
  等变体照旧。
- **T9 收尾修复后的补拍**：A1–A9 改动涉及的场景以 `--only terminal-banner*,tabs*,startup*,forwards*,settings*` 重拍：
  25 个场景（含新增的 `forwards-host-picker`）、125 张全部拍成，manifest `status` 为 ok、`failures` 为空，用时约 7 分钟。
  全量运行因此是 92 个场景、466 张（`a_full_run_shoots_every_scenario_in_both_palettes` 断言计划不重名、每个场景四类齐全）。
- **人工抽查**：T9 抽查中发现并修复了三处：设计目录在 200% 文字时四列互相挤压（改为每列有最小宽度、放不下时换行，
  `every_section_of_the_catalogue_stays_inside_its_column` 测试量列不越界不重叠）；720×480 @200% 左右分屏时窗格头的缩放 / 关闭按钮被窗格边裁掉
  （名字与“活动”标记让位，`a_narrow_split_keeps_its_pane_heads_buttons_inside_the_pane`）；导入面板打开期间切换语言，
  两个输入框的占位文字不跟随（`the_import_panels_fields_follow_a_language_switch`）。终版重拍后再抽查
  `design-catalog`（深色 200%）、`home-hosts`（深色中文）、`home-narrow`（720×480 @200%）、`startup-failed`（浅色中文）、
  `terminal-selection`（浅色中文，放大核对选区从 “42” 的 “4” 起、到 “部署完成” 末字止）、`terminal-selection-block`
  （深色 200%）、`terminal-banner-long`（720×480 @200%）、`settings`（深色 200%）、`files-dual`（720×480 @200%）、
  `overlay-palette`（浅色中文）、`batch-running`（深色 200%）、`desktop-rdp`（720×480 @200%），没有重叠，截断都是带省略号
  的设计截断（标签名、地址、横幅句子）。看到但未改的见 7.6。
- **截图证明不了的**：动效（缩放、缓动、淡入淡出与错开）、拖动阈值、Shift、窗口级捕获与自动滚动、键盘路径；这些由 GPUI 测试覆盖
  （各节“测试”条目）。截图读回的是隐藏窗口的 GPU 后备缓冲，不经过 DWM。
- **启动实测**（T9 重测，Windows、真实数据副本、开发与发布构建各三次，表见 §1.5“阶段耗时”）：四阶段合计开发约 16.5ms、
  发布约 10–13ms；首页两者都在约 250–270ms 出现，由 250ms 品牌入场决定。未测进程启动到首帧的时间。

### 7.5 发布流程与 Actions 运行（R6，T2）

- CI（三平台原生构建 + 原生桌面测试，代码终版 482e987）：<https://github.com/sakullla/shellune/actions/runs/44331740848>
  ——Windows 全部步骤通过；macOS arm64 与 Linux x86_64 原生构建、Format、Clippy、原生桌面测试（`desktop_native` 30/30、
  `rdp_vnc` 3/3，`SHELLUNE_REQUIRE_NATIVE=1`）通过，Test 步骤当时因 `src/app/tests.rs` 的 3–4 个旧用例失败
  （`an_ended_shell_*` 三个状态卡用例与 macOS 上的 `a_delete_question_keeps_every_key_but_escape`）。这些用例已随 T6 的横幅
  与 T8 的按平台快捷键读取改写。Owner 把 T1–T8 的提交（f6af526）推到 `ci/ui-redo-check` 重跑三平台 CI：
  <https://github.com/sakullla/shellune/actions/runs/44443090557>。三平台全部通过，macOS 上 874 个界面测试全部通过。macOS 首轮有一次
  `tests/serial.rs` 的 `an_unplugged_device_is_reported_as_a_disconnect_and_the_port_is_released` 失败，原因是状态仍为 Open，
  属于时序偶发；该串口代码本次重做没有改动，重跑失败作业后通过。
  交付候选 365c54c（含 T9）也跑了一次 CI：<https://github.com/sakullla/shellune/actions/runs/44472528511>，三平台全部通过。
- Release 演练（workflow_dispatch，ec4fd74，不发布）：<https://github.com/sakullla/shellune/actions/runs/44328823642>
  ——三平台成功，便携 zip、`.app` 与 deb 都带 `third-party/` 许可证。
- 文档补交（8cc9f92）的 CI：<https://github.com/sakullla/shellune/actions/runs/44334569122>。

### 7.6 T9 收尾修复与仍未关闭的事项

T9 关闭了各任务审查留下的这些事项（每项有测试）：横幅详情在 720×480 @200% 几乎占满窗格（A1，限高 40%）；窄横幅的
“重新连接”图标与其他图标同色（A2，`primary_icon_button`）；关闭横幅后重连、取消，横幅保持隐藏（A3）；720×480 @200%
唯一的标签只显示一个字母（A4）；启动失败页的按钮需要滚动才能看到、进度条在阶段间向前跳（A5、A6）；首页分组改名 / 删除后
焦点句柄不清理（A7，`prune_group_focus`）；上传“将保存到 …”缺测试、隧道表单主机选择器展开态没有截图（A8）；设置导航用
方向键移动时不画焦点环（A9）。Owner 验收时另修一处：720×480 @200% 下，启动失败页的品牌占满首屏，失败标题被挤到滚动区外。
现在停住时品牌会退后：logo 变为 `BRAND_MARK_STOPPED` 40，产品名用标题字号，保证“哪一阶段失败”出现在按钮上方；
失败原因可滚动查看（由 `the_stop_scrolls_to_its_buttons_in_the_smallest_window_at_the_largest_text` 断言）。

仍未关闭（均非阻断）：

- 启动失败页：在 720×480 @200%（逻辑 360×240）下，首屏只显示失败标题和两个按钮，原因与提示要滚动才能看到，而且没有“下方还有内容”的滚动提示。
- 截图矩阵：92 个场景中有 56 个没有 720×480 @200% 的截图，只有 1120×720 @200% 的截图；最小窗口下是否溢出由 GPUI 测试检查。

- 文件视图：720×480 @200%（相当于 360×240 的逻辑空间）时两栏头部、面包屑与底栏几乎占满高度，列表区只剩很少的空间。
- 启动失败：原因句取自存储层，界面为中文时也是英文（`startup.failure.reason`；标题与说明随语言）。
- 终端选择：阈值之前锚点按视口坐标记录，流式输出期间缓慢越过阈值时锚点可能落到新内容上；`pressed_button ≠ Left` 的松开分支与
  向下自动滚动没有专门测试。
- 主机 / 连接表单：`ConnectionFormField::is_basic` 与对话框各自的分组判断重复，靠 `*_tab_follows_the_drawn_order` 测试兜底。
- 发布：POSIX 上 VNC 套接字未设 close-on-exec。
- 串口集成测试 `an_unplugged_device_is_reported_as_a_disconnect_and_the_port_is_released` 在 macOS CI 上偶发失败（见 7.5），属于既有的时序问题。
