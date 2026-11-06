# Windows 视觉材料（0.1.0）

本目录的 PNG 是在 Windows 11 x86_64 上运行 `cargo build --release` 产物时，用
`GetWindowRect` + 屏幕抓取（DPI 感知进程）截取的窗口整幅（1702×1136 物理像素，
含 11 px 边框与 45 px 标题栏；客户区 1680×1080 物理像素 = 1120×720 逻辑像素，
DPI 144 / 150% 缩放）。截图只经过无损保存，没有缩放、裁剪或重采样，审阅者可以按
原始像素检查字距、截断、遮挡与对比度。

## 抓取方式

1. 启动 release 产物 `shellune.exe`（本轮重新打包的
   `dist/Shellune-0.1.0-windows-x86_64-portable/shellune.exe` 同源）。
2. 用 `SendInput` 级的 `keybd_event` 与 `WM_CHAR` 投递真实键盘事件；点击用
   `mouse_event` 投递到客户区坐标。
3. 每一步之后抓取一张，按键与命令内容写在文件名里。

因此这些图是"人在窗口里操作"的等价物，不是测试渲染管线生成的图，也不是手工 PS 的图。

## 文件

| 文件 | 画面内容 | 应检查什么 |
| --- | --- | --- |
| `windows-r8-default-shell-prompt.png` | 出厂默认本地 Shell（Windows PowerShell）启动后的首屏 | 提示符、光标、边距，无截断 |
| `windows-r8-ping-running.png` | 终端里 `ping -n 60 127.0.0.1` 正在输出 | 中文 Ping 文案与 TTL 行是否对齐、有无被裁切 |
| `windows-r8-ping-after-ctrl-c.png` | 按下 Ctrl+C 之后：`Control-C` 与提示符 | 提示符已回来，历史输出未被破坏 |
| `windows-r8-next-command-ran.png` | 中断后立即执行的下一条命令 | 命令被执行、输出完整 |
| `windows-r8-cmdping-running.png` | `cmd /c ping -n 60 127.0.0.1` 运行中 | 与上同，另一种把程序放到前台的写法 |
| `windows-r8-cmdping-after-ctrl-c.png` | 同上，Ctrl+C 之后 | `^C` 与控制权回到 PowerShell |
| `windows-r8-vim-started.png` | MSYS Vim 启动（alternate screen） | 全屏接管是否完整、状态行是否被裁切 |
| `windows-r8-vim-insert-mode.png` | Vim 插入模式，键入 `vim was here` | 光标位置、`-- INSERT --` 行 |
| `windows-r8-vim-after-quit.png` | `:q!` 退出后 | alternate screen 是否彻底还原 |
| `windows-r8-vim-shell-back.png` | 退出 Vim 后再执行 `echo after-vim` | Shell 与屏幕内容正常，无残留字符 |
| `windows-r8-top-wsl.png` | `wsl top`（Debian 13，WSL2） | 表格列是否对齐、被截断的 `COMMAND` 列 |
| `windows-r8-top-wsl-after-quit.png` | `q` 退出 top 之后 | 是否回到提示符 |
| `windows-r8-tmux-wsl.png` | `wsl tmux` 会话与状态栏 | 状态栏、边框线是否连续 |
| `windows-r8-tmux-wsl-after-exit.png` | 退出 tmux 会话之后 | 是否回到提示符 |
| `windows-r8-ime-composing.png` | 微软拼音输入 `nihao` 组合中 | 组合过程在终端内的显示方式 |
| `windows-r8-ime-committed.png` | 候选上屏（`你好`）之后 | 上屏字符是否正确落位 |
| `windows-r8-ime-output.png` | 提交整行后 PowerShell 的回显 | 中文是否被正确回显，无乱码 |

## 仍然没有的东西

macOS 13+ aarch64 与 Ubuntu 22.04 x86_64 的截图**不存在**：本机没有这两个环境，
本轮也没有运行它们。`docs/release/0.1.0-verification.md` 把它们标为未验证。
