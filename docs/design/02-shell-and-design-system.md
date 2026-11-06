# 应用外壳与设计系统

本文件是 `src/app/theme.rs`、`src/app/components.rs` 与 `src/app/view.rs` 的设计依据。
tokens 只在 `theme.rs` 定义一次，文档与代码不一致时以代码为准并同时修正本文件。

## 1. 结构：导航栏 + 标签条 + 对象区

四种对象（本地终端、主机、会话、传输）之间是平级切换，因此保留"单一导航栏 + 单一对象区"；
终端打开后，对象区让位给终端，会话以标签呈现。

```
+-------------+---------------------------------------------------+
| >_ Shellune | 本地终端                     [新建本地终端] [v]     |  头部：标题 + 摘要/数量 + 主按钮
|  Shell-loon | 在本机上运行的 Shell · 2 个会话                      |
|             +---------------------------------------------------+
| | 本地终端 2 | (o PowerShell 1 x) (o SSH prod x) (+)              |  标签条：仅在有会话时出现
|   主机 Ctrl+2| +-----------------------------------------------+ |
|   会话    2 | | o 本地 PowerShell powershell.exe [已连接] 关闭   | |  会话栏：连接状态与操作
|   传输 Ctrl+4| | PS C:\>                                        | |  终端网格（等宽字体）
|             | | >_ PowerShell         A- A+ 复制 | 105x27 14px  | |  终端状态行：进程与显示
| 文字大小 100%| +-----------------------------------------------+ |
+-------------+---------------------------------------------------+
| o 2 个会话                              语言 · 中文 | Shellune 0.1.0 |  状态栏
+-----------------------------------------------------------------+
```

- 空的本地终端页显示状态卡片和 Shell 磁贴；点击磁贴立即打开该 Shell，并记为主按钮的默认 Shell。
- 终端打开后（以及会话页），磁贴收进头部主按钮旁的菜单；在菜单里选 Shell 即开新标签。
  "自定义路径"在菜单内展开输入框，启动失败时输入框与菜单保留，便于修改路径。
- 会话页的主按钮也打开本地 Shell；传输页的主按钮在有可浏览的连接时打开其文件视图，
  否则转到主机页。没有任何主按钮是"点了也不做事"的。
- 标签只显示状态点、远程标记（SSH）、名称和关闭按钮；非"已连接"时才写出状态文字。
  同名标签按打开顺序编号（"PowerShell 1"、"PowerShell 2"），完整的对象与状态在标签提示里。
- 列表页（主机、空状态、传输行）是最宽 960px 的居中阅读栏；终端不受此限，铺满对象区。

被否决的候选与理由见 `01-task-flows.md`；分屏仍按 R14 不做。

## 2. 视觉 tokens

颜色以 24 位 sRGB 常量给出，文字与状态色对四种表面的对比度由测试校验（WCAG AA：正文
4.5:1，图形 3:1）。

| token | 值 | 用途 |
| --- | --- | --- |
| canvas | `#0c0e13` | 对象区、终端背景（与 `shellune-terminal` 的 BACKGROUND 一致） |
| surface | `#111419` | 导航栏、标签条、会话栏、状态栏 |
| surface raised | `#171b22` | 卡片、行、按钮 |
| surface hover | `#1c212a` | 指针悬停 |
| surface selected | `#212734` | 选中的导航项、标签、分段项 |
| border / border strong | `#232934` / `#343c4b` | 分隔线 / 控件边框与悬停边框 |
| text primary / secondary / muted | `#edf0f6` / `#b6bece` / `#939db0` | 标题 / 正文 / 提示与计数 |
| accent / accent hover / on accent | `#9db4ff` / `#b7c8ff` / `#0b1022` | 主按钮填充、焦点环、选中标记 |
| accent tint / on tint | `#222b45` / `#c8d5ff` | 次级强调（Soft 按钮、SSH 标记、徽标） |
| positive / warning / danger | `#64d8a8` / `#f3c66c` / `#ff8d99` | 状态色，界面里只有它们是饱和色 |
| positive / warning / danger tint | `#14271f` / `#2a2315` / `#2d1a1f` | 状态 chip 与提示条的底色 |
| planned | `#9aa6bd` | 已关闭会话 |

字号：caption 11、label 12、body 13、title 15、heading 18；圆角 6 / 8 / 12；
间距 4 / 8 / 12 / 16 / 24。控件高度 30（紧凑 26），标签 30，导航项 34，列表行至少 52。
所有长度经 `Metrics` 按文字大小（90% / 100% / 115%）缩放。

终端网格使用平台等宽字体（Windows Cascadia Mono、macOS Menlo、Linux DejaVu Sans Mono），
与测量单元格所用字体相同，保证列对齐与光标位置一致。

## 3. 状态语言

一个分区只能处于四种状态之一，映射集中在 `workspace::SectionState`：

| 状态 | 色调 | 呈现 |
| --- | --- | --- |
| 加载中 | warning | 状态卡片；头部标题旁有"加载中"chip |
| 失败 | danger | 状态卡片 + 原因 + 建议下一步；头部标题旁有"失败"chip |
| 空 | neutral | 状态卡片 + 下一步（与主按钮同名） |
| 有内容 | - | 行列表；头部摘要行写出数量（"2 个会话"） |

规则：

1. 只有"有内容"可以表现为成功；"已就绪"不再单独做 chip，数量即是结果。
2. 会话色调只由 `SessionState` 决定，`Connected` 是唯一 Positive；导航点只在其中有已连接会话时为绿色。
3. 提示、失败、确认问题都用同一个 `banner`：色调底色 + 左侧色条，文字保持主文字色。

## 4. 键盘、焦点与缩放

| 动作 | Windows / Linux | macOS |
| --- | --- | --- |
| 切换分区 | Ctrl+1...4 | Cmd+1...4 |
| 下一个 / 上一个分区 | Ctrl+Tab / Ctrl+Shift+Tab | Cmd+Tab / Cmd+Shift+Tab |
| 新建本地终端（终端内也有效） | Ctrl+Shift+T | Cmd+T |
| 文字大小 | Ctrl+= | Cmd+= |
| 界面语言（终端外） | Ctrl+L | Cmd+L |
| 关闭菜单 / 提示条（终端外） | Esc | Esc |

终端内 Esc 与 Ctrl+L 属于终端程序；语言可点状态栏切换。焦点环为 accent 边框。

## 5. 组件清单（`components.rs`）

| 组件 | 用途 |
| --- | --- |
| `button(label, Variant)` / `compact_button` | Primary（实心）、Soft（强调底色）、Secondary、Ghost、Danger 五级 |
| `icon_button` | A-、A+、关闭、新建等短符号按钮 |
| `segmented` + `segment` | 主机视图、登录方式等互斥选择 |
| `card` / `card_title` / `field_row` | 表单与面板（主机编辑、导入、远程连接） |
| `banner` | 提示、失败、确认问题、操作结果 |
| `state_card` | 加载中 / 失败 / 空 |
| `row` + `stacked` | 主机、会话、传输行（悬停高亮） |
| `chip` / `badge` / `kind_badge` / `keycap` / `status_dot` | 状态、数量、类型、快捷键 |
| `Tooltip` / `tooltip()` | 图标按钮与标签的悬停说明 |

新增分区必须复用同一组组件与 `SectionState`，不为某个分区单独发明状态表达。

## 6. 文案

- 产品名与两句简介由 `product.rs` 提供；简介按界面语言显示在空的本地终端页。
- 其余文案全部在 `copy.rs` 的双语表中；英文计数用 `copy::counted` 处理单复数。