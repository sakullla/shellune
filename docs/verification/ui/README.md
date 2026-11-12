# Shellune UI capture 基线

这里放的是真实窗口（隐藏创建、GPU 后缓冲读回）画出来的界面像素，以及描述它们的 manifest 和画廊。
捕获工具是 `shellune --ui-capture`，命令入口是 `scripts/capture-ui.py`；两者都不是行为测试：一张 PNG
只证明"这一版把这些像素画成了这样"。

## 轮次

每个轮次发布在自己的目录里，只替换自己，绝不动别的轮次：

| 命令 | 轮次目录 | 内容 |
| --- | --- | --- |
| `python scripts/capture-ui.py` | `docs/verification/ui/`（根） | 完整矩阵：21 张，两种主题、两种尺寸、100%/115% 文字、中英 |
| `python scripts/capture-ui.py --quick` | `docs/verification/ui/quick/` | 三个主页面的深色默认尺寸 |
| `python scripts/capture-ui.py --only "home-hosts"` | `docs/verification/ui/only-home-hosts/` | 匹配场景的全部变体 |
| `--theme` / `--size` | `theme-light/`、`size-720x480/` 等 | 该筛选下的全部变体 |

根目录始终是**完整轮次**：一次 `--quick` 只会替换 `quick/`，所以它不可能删掉自己没有拍的那十几张基线。
每份画廊顶部会列出旁边的其他轮次，`index.html` 可以逐页打开对照。

## 发布规则

1. 先在 staging 目录里校验并**无损**重压缩全部 PNG，再把 manifest 和画廊写进 staging；
2. 整轮就位（同盘 rename，逐个文件原子），被替换的旧文件先备份，任何一步失败就整轮回滚；
3. 只有在整轮就位之后，才删除上一轮登记过、这一轮没有发布的图像。

任何一步失败都不会改动已发布的轮次：失败的那一轮会留在 `.failed-<时间戳>/` 里作为诊断，
命令可以直接重跑。`python scripts/capture-ui.py --self-test` 不需要窗口、GPU 或编译就能复验这套规则
（含"一张坏图不发布"和"移动到一半失败要还原"两个失败用例）。

## 本轮（2026-11-12）的变化

- 基线像素只在五个位置变了：`home-hosts-1120x720-dark`、`-light`、`-zh` 与
  `overlay-palette-1120x720-dark`、`-light`。原因是主机页底部会画出"记录存放在 <路径>"，
  而该路径的目录名里曾经带着这个 shot 在本轮计划里的序号（`06-home-hosts-...`）。同一个 shot 在
  `--quick`（第 1 个）和完整轮次（第 7 个）里因此差出一个 9×12 像素的字形。
  现在每个 shot 的隔离目录只以 shot 自己命名，序号不再进入画面。
- 新增 `quick/` 和 `only-home-hosts/` 两个轮次，作为模式隔离的样例与复验对象。
- 复验：完整轮次连跑两次全部 21 张字节一致；`quick` 与 `only-home-hosts` 里每个同名 shot 的像素
  与完整轮次完全一致（manifest 的 `pixel_crc32` 相同）。

## 证据边界

- 只有 Windows 的 DirectX 后缓冲读回被实测；macOS 与 Linux 的隐藏窗口捕获仍未验证。
- 文字用这台机器上安装的字体渲染，跨机器的像素比较没有意义。
- 会话是只读 fixture，没有启动任何 shell、连接或用户命令；截图不证明这些页面可以工作。
- 基线尚未逐张人眼评审。
