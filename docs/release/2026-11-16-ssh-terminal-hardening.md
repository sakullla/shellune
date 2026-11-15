# SSH、本地终端与日志刷屏优化

## 修复内容

- SSH 直连、跳板链和本地 PTY 的输出改为有界积压。SSH 数据预算为 1 MiB，小消息同时计入队列开销；本地队列为 128 个、每个最多 8 KiB 的读取块。满时施加背压，不丢弃终端数据。协议库、内核缓冲和正在处理的一批数据另计，这不是整个应用的内存上限。
- 每次界面轮询最多取约 64 KiB / 64 个事件；相邻数据合并解析，退出和状态事件保持原顺序。持续输出时搜索最多每 100ms 重算，查询编辑、导航仍立即刷新，退出前刷新最后一批结果。
- 本地 PTY 写入移到后台线程。SSH 写入独立于 shell 的读取循环，等待远端窗口时仍能接收输出。输入预算为每会话 1 MiB，包含正在发送的数据；超限整次拒绝，界面明确提示本次输入未发送，连接保持可用。
- SSH 关闭不再排队等大段输入或握手超时：直连给正常取消/断开最多 250ms，跳板链给清理最多 1 秒，然后退出运行时。正常的主机身份未决、跳板取消结果保留；证书、认证和 keepalive 策略没有放宽。
- 本地退出等待识别读取线程正在等队列空间的状态，避免慢消费者下退出事件抢在已读输出前；关闭时释放接收端，唤醒受背压阻塞的线程。
- OSC 分段改为游标扫描后一次移除前缀，避免彩色输出和大量转义序列造成反复搬动剩余缓冲。滚动位置跟踪复用行地址缓冲，减少分配。
- 命令块预览保留最近约 128 行、最多 64 KiB 的输出尾部，省略部分以 `[…]` 标记；完成记录最多 1000 条、文本总量 8 MiB，超限移除最旧记录。经典终端回滚继续遵守用户原有设置，命令重跑文本不截断。按 ID 查找及搜索不再先复制所有记录。

## 验证

Windows 客户端使用项目锁定的 russh 0.64.1；远端为用户提供的 `192.168.0.109` Linux/OpenSSH。使用已有密钥认证，并预先核对主机指纹；未更改远端 SSH 服务或系统网络规则。

| 场景 | 本次结果 |
| --- | --- |
| 真实 SSH，100 万行，主动暂停消费 3 秒 | 7.744 秒完成；逐行计数完整，无丢失或重复；独立探针采样峰值私有内存 4,595,712 字节 |
| 真实 SSH，双向各延迟 5 秒，2 万行 | 60.324 秒连接，80.877 秒完成；逐行计数完整 |
| Windows ConPTY，生成 100 万行，暂停消费 2 秒 | 3.590 秒完成；末尾标记在退出事件前可见，回滚 10,000 行 |
| Windows ConPTY，生成 1000 万行，同样暂停 | 21.495 秒完成；末尾及退出顺序正确，回滚仍为 10,000 行；独立探针采样峰值私有内存 36,798,464 字节 |

ConPTY 自身会转换、合并输出；本地测试验证最终画面标记、退出顺序和回滚上限，不声称逐字节接收了子进程生成的全部文本。内存为每 50ms 采样的探针进程私有内存，不含子进程、完整 GPUI 应用或 GPU，也不是长期 RSS 基准。

纯解析对照：同样 120×40、10,000 行回滚、16 MiB 普通日志、8 KiB 分块，开发构建的命令块预览保留量从 727,380 字节降到 9,381 字节。单次最长 feed 从 101.61ms 降到 19.74ms，总耗时从 5.890 秒到 5.324 秒。机器同时有其他构建，数字仅作单次观测，不作为稳定加速比例或 UI 帧率承诺。

回归包含：输入预算覆盖发送中数据、慢消费保持字节及事件顺序、关闭唤醒阻塞读取、退出不越过背压、阻塞 PTY 写入不阻塞调用方、静默 SSH 握手可取消、命令记录与 UTF-8 截断边界。核心 167 项、SSH 82 项、终端 137 项、应用 635 项测试通过，核心/SSH/终端 all-targets Clippy 无警告。

既有 Ctrl+C 集成测试另有 5 项通过，设置 `SHELLUNE_REQUIRE_INTERRUPT_DELIVERY=1`，无跳过；持续输出与 shell 转发场景均第一次按键收到标记，停止时间约 31–36ms。由于正在运行的应用占用了共享构建目录的 EXE，这 5 项通过独立 Cargo harness 引用原测试文件及原 flood helper 运行，没有关闭用户应用。

## 复现

```powershell
cargo test -p shellune-core -p shellune-ssh -p shellune-terminal --lib --features shellune-ssh/test-support
cargo test --bin shellune
cargo test --test local_interrupt --features test-support
cargo clippy -p shellune-core -p shellune-ssh -p shellune-terminal --all-targets --features shellune-ssh/test-support -- -D warnings
cargo run -p shellune-terminal --example flood -- 16 8192
cargo run -p shellune-terminal --example local_probe -- 1000000
# 先设置 SHELLUNE_SSH_PROBE_KEY 和已核实的 SHELLUNE_SSH_PROBE_FINGERPRINT。
cargo run -p shellune-ssh --example terminal_probe -- HOST PORT USER 1000000 3
```

高延迟使用已有的 `scripts/protocol-latency-proxy.py`，两个方向各延迟 5 秒。SSH 探针明确把连接预算设为 180 秒；正式会话仍遵守用户的连接超时和 keepalive 设置，默认 20 秒不保证能完成 10 秒 RTT 的多轮握手。测试未覆盖丢包、带宽限制、半开连接长期恢复、所有跳板拓扑和完整 UI 帧率。
