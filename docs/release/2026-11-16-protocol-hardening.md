# 串口、RDP、VNC 修复与高延迟验证

同步基线：`5147145`，合并远端 0.4.5 与本地改动后已先行推送。输入入口同时保留串口换行/回显和输入时显示光标；同步版 411 个界面测试通过。

## 修复

- RDP 独立启动时初始化并配对释放 Winsock；不再依赖其他模块先打开 socket，也不再调用控制台凭据输入。
- 修正证书决定映射：shim 的 `2` 是拒绝，FreeRDP 的 `2` 却是临时接受。现在拒绝返回 `0`，接受仅对当前连接有效，拒绝报告 `CertificateRejected`。
- VNC 使用界面提供的密码回调；静态 OpenSSL 使用 `no-module` 将 legacy provider 编入 libcrypto，供 VNC 密码认证所需的 DES 使用。
- VNC 先消费 LibVNCClient 已缓冲的数据，防止剪贴板/首帧一起到达时画面等到下一次 socket 数据甚至断开才出现。此前忽略的首帧用例已恢复。
- VNC 释放重设尺寸和退出时的应用帧缓冲；不再重复发送初始全屏请求；优先压缩编码，启用 TCP_NODELAY。
- VNC 的 32 位像素中第四字节是填充，不是透明度；显示层统一置为不透明，避免收到像素却显示不出图像。
- 串口发送被队列拒绝时不再回显未发送文字，保持连接和发送失败提示。
- RDP 不支持的剪贴板请求不再永久堵住后续键盘事件；暂时性队列满仍按原顺序重试。

## 性能与内存

- 待消费帧合成到一块画布，不按连接时长积累矩形列表。保留最新像素、脏矩形、状态与当前证书请求；通知数量限制为 128，合并未读取的剪贴板更新。
- 画布像素预算为 64 MiB；请求尺寸、RDP 初始化和显示层均检查预算。这个预算不是整个进程的 RSS 上限：解码、显示层、交付快照和 GPU 仍各有缓冲。
- 脏矩形逐行紧密复制，不复制桌面 stride 的空隙。测试机同一个 500×203 更新由 648,400 字节降为 406,000 字节。
- 鼠标连续移动在 Rust 和 native 队列中合并，保留按钮变化、按键和滚轮边界。长按键不再反复增加“仍按住”的记录。
- 串口空闲写线程和接收背压改为条件变量通知，去掉 5ms 周期唤醒；关闭时在对应锁下通知，避免丢失唤醒。
- 构建脚本每条命令失败立即退出，避免 Configure 失败后错误地给旧静态库写入新版本标记；OpenSSL 使用 `/Z7` 避免共享 PDB 写入失败。

## 测试环境与结果

Windows 客户端链接 FreeRDP 3.33.1、LibVNCClient 0.9.15、OpenSSL 3.5.4。服务端是用户提供的 Debian 13 测试机，使用独立 TigerVNC 显示、xrdp TLS 监听器和 SSH 本地转发。没有修改主机网络规则或既有服务。

延迟代理在两个方向分别延迟 3 秒或 5 秒，近似 6 秒或 10 秒应用层 RTT；它不是丢包、带宽限制或全协议网络仿真。以下是一次实测的首个非空帧，不代表整屏完成或输入到画面的延迟：

| 协议 | 附加 RTT | 首帧 | 取消后 worker 退出 |
| --- | ---: | ---: | ---: |
| VNC | 6 秒 | 33.155 秒 | 1ms |
| VNC | 10 秒 | 55.175 秒 | 1ms |
| RDP / TLS | 6 秒 | 72.319 秒 | 3ms |
| RDP / TLS | 10 秒 | 120.261 秒 | 12ms |

VNC 握手每次读取等待从 2 秒调为 30 秒，TCP 连接预算为 30 秒。RDP 激活预算由库默认的 9 秒改为 120 秒。取消仍通过 socket shutdown / FreeRDP 中断事件进行，不等待这些预算耗尽。

实际拒绝 xrdp 自签名证书后返回 `CertificateRejected`，没有连接成功或帧事件。Linux 的真实 PTY 通过 termios 收发全部 256 种字节，3 秒延迟回复后数据完整，空闲读取可中断关闭。Windows 和 Linux 分别通过串口单元测试；Linux PTY 不等同于 Windows COM 驱动或物理 UART。

回归覆盖 100,000 次慢消费者帧更新、脏区域之间未改变的像素、紧密帧复制、超大尺寸拒绝、VNC 延迟 banner、剪贴板与首帧一起到达、关闭竞态、鼠标事件顺序、键盘长按、串口队列拒绝回显。

完整应用回归：626 通过、0 失败。串口单元测试：Windows 18 通过，Linux 18 通过；Linux 另有 1 个真实 PTY 测试通过。

最终静态库的 TigerVNC 密码认证实测通过：0.407 秒收到首帧，取消耗时 1ms。桌面后端 5 个单元测试、15 个原生协议测试全部通过；桌面/串口 Clippy 检查通过。原生构建脚本的版本标记与失败立即退出检查通过。

## 复现入口

```powershell
python packaging/native/build_native.py --perl C:/path/to/perl.exe
cargo test -p shellune-desktop
cargo test -p shellune-serial --features test-support
cargo test --bin shellune
cargo run -p shellune-desktop --example probe -- vnc 127.0.0.1 15991 180
cargo run -p shellune-desktop --example probe -- rdp 127.0.0.1 13389 180
```

`probe` 只用于明确选择的测试端点，会接受其证书一次。密码通过 `SHELLUNE_PROBE_PASSWORD` 环境变量提供，不写日志；设置 `SHELLUNE_PROBE_REJECT_CERTIFICATE=1` 改为验证拒绝路径。延迟代理：

```sh
python3 scripts/protocol-latency-proxy.py --listen 15993 --upstream 15991 --delay 3
```

剩余边界：未验证实体串口的流控、拔插、DTR/RTS，未验证 Windows Server/NLA 和所有 VNC 安全类型、编码组合，也未做长期 RSS 或完整 UI 帧率基准。RDP 通过项使用 TLS；xrdp 的旧式 RDP 安全层仍出现服务端 MAC 校验错误，尚未解决，不列为通过。超高延迟下仍受协议握手往返次数影响，不能把这些首帧结果解释为交互流畅。RDP 剪贴板仍不受支持，会明确报告。
