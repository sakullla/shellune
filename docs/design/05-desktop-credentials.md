# 远程桌面凭据（RDP / VNC）

> 设计说明，配合 `02-shell-and-design-system.md` 的「应用内远程桌面与串口会话」一节阅读。
> 这里只描述 RDP / VNC 的密码从表单到客户端的流向与「记住密码」的语义；串口与 SSH 不变。

## 1. 表单

RDP 与 VNC 的连接表单多一行密码（`ConnectionFormField::Secret`，选择器 `connection.field.secret`），
标签复用 `strings.auth_password`，输入框与 SSH 主机的密码框一样是遮罩的（`set_masked` + `set_revealable`）。
它属于「基本信息」组，Tab 顺序是 地址 → 端口 →（RDP：用户 → 域）→ 密码 → 名称 → 分组，再到「选项」组；
串口表单没有这一行。

同一行下面是一个「记住密码」勾选框（`connection.field.remember`），文案与语义都复用主机表单的
`host_remember_password` / `host_remember_note`：**勾选**时密码进系统凭据库（Keychain / 凭据管理器），
**不勾选**时密码只在本次运行的内存里等待这次连接使用，不落盘、不写入记录。

编辑一条已保存的记录时，密码框总是空的：已保存的密码不会被读回来显示，占位符
（`host_secret_saved_placeholder`）说明「已保存；留空则保持不变」。留空保存不改变凭据库里的值，
输入新值则覆盖。

## 2. 保存

`ProtocolDraft` 分别持有 `secret`（本次输入）、`remember`（勾选状态）与 `has_saved_secret`（打开编辑时
记录里的 `remembers_secret()`）。记录本身不含密码，只有 `remembers_secret` 这个布尔：

- `keeps_secret() = remember && (输入了密码 || 记录里已有一个)`，写入记录的 `set_remember_secret` 用的就是它，
  所以「勾选了但什么都没输入、也没有已保存的值」不会让记录谎称凭据库里有密码。
- 保存时（`ShelluneView::store_connection_secret`）：记录说保留 → `secrets.save(host.id(), …)`；
  记录说不保留但之前保留过 → `secrets.forget(host.id())`。
- 不保留但输入了密码 → 放进 `ShelluneView::desktop_secrets`（`HostId` → `SecretString` 的内存表），
  等这次连接把它取走；再次保存该记录会覆盖或清掉它，删除该记录也会清掉它。

## 3. 连接

`ShelluneView::desktop_password` 是唯一的取值处，按顺序：

1. 内存表里刚输入、未保存的密码（取走即删，只给一次启动）；
2. 记录 `remembers_secret()` 为真时 `secrets.load(record.id())`；
3. 都没有则 `None`。

`start_desktop_session` 把它交给 `DesktopPane::start(password)` → `NativeLink::open` →
`DesktopClient::set_credentials(user, password, domain)`：RDP 用 user / domain / password，
VNC 只用 password（VncAuth）。密码不进入 `DesktopTarget`、不写日志、不显示在状态卡或提示条里；
服务器拒绝时，状态卡显示的是协议层返回的原因。重连走同一条 `start_desktop_session`，所以第 2 步对重连同样成立。

```
表单密码 ─┬─ 勾选「记住」→ secrets.save ─┐
          └─ 未勾选     → desktop_secrets ─┴→ desktop_password → DesktopPane::start → NativeLink::open → set_credentials
```

## 4. 测试

`src/app/tests.rs`：

- `the_desktop_form_asks_for_a_password_and_hides_it`：RDP/VNC 有密码行且遮罩，串口没有。
- `a_desktop_connection_starts_with_the_password_typed_in_its_form`：不勾选「记住」时，启动收到刚输入的密码
  （`DesktopPane::started_password`）。
- `a_kept_desktop_password_is_stored_and_forgotten_with_the_box`：勾选 → 凭据库有、记录文件没有；
  启动从凭据库取；取消勾选 → 凭据库删掉、记录不再宣称保留。
- `a_saved_desktop_password_is_never_shown_back_and_a_blank_field_keeps_it`：编辑时框为空，留空保存沿用旧值。
- `switching_protocols_does_not_carry_a_password_between_forms`：SSH → RDP → VNC → SSH 之间互不串用。
- `the_desktop_form_asks_for_a_password_and_hides_it` 同时断言草稿里的文本正好是输入的那串，
  每个字符只落一次。
- `one_press_on_a_saved_desktop_opens_one_attempt`：一次「连接」只启动一次
  （`ShelluneView::desktop_starts`、`DesktopPane::starts`），尝试失败后也不会悄悄再启动。
- `every_keystroke_reaches_a_desktop_once`：一个按键只产生一次按下与一次抬起，输入不会成对下发。

## 5. 与「登录了两次 / 会话断开」有关的事实

- 应用侧一次「连接」只有一次启动（上面两个用例），列表按钮、双击行、重连都走
  `start_desktop_session`；失败后不自动重启，另一次尝试由用户发起。
- **协议层不自己重连**（`crates/desktop/src/rdp/session.rs` 的 `run`）：会话被服务器关闭
  （`RemoteClosed`）时直接以 `State::Failed` 结束，并带上协议层的原因；静默重连会让服务端把同一
  用户记成第二条登录（xrdp 会接管同一 display 并踢掉第一个连接），也把“会话断了”藏起来。
  重新连接是用户的动作：标签菜单、快捷键或列表行的连接会为同一标签开一个新的尝试（新 pane、新 link），
  由 `a_dropped_desktop_reconnects_only_when_the_user_asks` 钉住。
- 键盘输入走慢路径 `TS_KEYBOARD_EVENT`，抬起标志是 `0x8000`（`KBD_FLAGS_RELEASE`）、扩展键是
  `0x0100`。用快速路径的 `0x01/0x02` 会让服务器把每次抬起当成第二次按下，于是“每个字符输入两次”；
  由 `a_slow_path_key_release_is_flagged_as_a_release` 直接解析 input PDU 的 keyboardFlags 钉住。
- 剪贴板要求服务器侧有 chansrv（xrdp 的 `/usr/lib/xrdp`）。chansrv 起不来的端点上 cliprdr 通道
  不会被授予，`NativeLink::open` 仍然按记录把 `set_clipboard(true)` 传下去，
  但本地内容无处可送，粘贴时窗格只会给出「发送失败」；这不是应用关掉了共享。
