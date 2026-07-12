# RemoteOps

RemoteOps 是一个面向远程系统维护和嵌入式 Linux 开发的 MCP 工具。服务分为 PC 端 proxy 和远端 agent。

```text
AI Agent <-- MCP stdio --> remote-ops-proxy
                              |
                              | authenticated TCP + binary frames
                              v
                       remote-ops-agent
                              |
                              v
                    remote filesystem / process / shell
```

## 功能

- MCP stdio 支持 `initialize`、`ping`、初始化通知、`tools/list` 和 `tools/call`。
- 保留旧版 12 个工具的名称、参数和结果结构。
- `upload_file`、`download_file` 直接在 PC 路径和远端路径之间传输任意二进制单文件。
- proxy 默认使用 `192.168.43.107:8022`，AI Agent 可查询连接状态并在运行时切换远端 IPv4 或端口。
- 文件按 64 KiB 分块传输，完成后校验总长度和 SHA-256。
- 上传和下载先写同目录临时文件，校验成功后原子替换；Unix 上保留已有目标文件的 mode 并同步父目录。
- proxy 与 agent 使用内置固定值 `JARK006_PSK` 建立连接；每个帧都有 HMAC-SHA256 和单调序号保护。
- 工具请求不会在连接状态不确定时自动重放，避免重复执行命令或破坏性操作。

## 平台支持

| 能力 | Linux | Windows | macOS |
| --- | --- | --- | --- |
| 连接、认证、二进制传输 | 支持 | 支持 | 支持 |
| 文本、目录、stat、哈希 | 支持 | 支持 | 支持 |
| `exec` | 支持 | 支持 | 支持 |
| `sh_exec` | 支持 | 返回 unsupported | 支持 |
| `kill` | 支持 | 支持 | 支持 |
| `pids`、`process_info` | 支持 | 支持 | 返回 unsupported |
| `system_info` | 支持 | 支持 | 返回 unsupported |

Linux 是首版完整功能目标。Windows 的 `kill` 将 signal 9 和默认值 15 映射为强制终止，不模拟 Unix 信号语义；Windows `exec` 使用 Job Object 约束命令进程树，超时会终止命令及其后代，调用结束后不会保留后台后代进程。Windows 的 `system_info` 提供主机、系统版本、运行时间、内存和系统盘信息；Windows 无 Unix load average 和统一温度接口，对应字段分别返回零值和空数组。

## 构建

```sh
cargo build --release --workspace
```

本机产物位于：

```text
target/release/remote-ops-proxy
target/release/remote-ops-agent
```

Linux 可使用 `cargo-zigbuild` 交叉编译：

```sh
cargo zigbuild -p remote-ops-agent --target armv7-unknown-linux-musleabi --release
cargo zigbuild -p remote-ops-agent --target aarch64-unknown-linux-musl --release
cargo zigbuild -p remote-ops-agent --target x86_64-unknown-linux-musl --release
```

## 启动远端 agent

Linux/macOS：

```sh
./remote-ops-agent --listen 0.0.0.0:8022
```

Windows PowerShell：

```powershell
.\remote-ops-agent.exe --listen 0.0.0.0:8022
```

agent 参数：

```text
--listen HOST:PORT           默认 0.0.0.0:8022
--timeout-ms N               socket I/O 超时，默认 30000
--max-transfer-bytes N       单文件上限，默认 4294967296（4 GiB）
```

agent 串行服务一条 proxy 连接。连接断开后会继续等待下一次连接。

## 配置 MCP proxy

Claude Code、Codex 等 AI Agent 通过 stdio 与 remote-ops-proxy 通信，而 remote-ops-proxy 通过网络与远程 remote-ops-agent 通信。通用 MCP 客户端配置示例：

```json
{
  "mcpServers": {
    "embedded-board": {
      "command": "D:\\tools\\remote-ops-proxy.exe",
      "args": []
    }
  }
}
```

proxy 参数：

```text
--remote IPv4:PORT           可选，默认 192.168.43.107:8022
--timeout-ms N               一次远端操作的 I/O 超时，默认 310000
--max-transfer-bytes N       单文件上限，默认 4294967296（4 GiB）
```

proxy 会在首次远端工具调用时建立连接，因此 `initialize` 和 `tools/list` 不要求远端在线。断线后的下一次调用会重新连接；已经发出的调用不会自动重放。

`remote_status` 只报告 proxy 当前是否持有已认证会话，不会主动探测网络；对端静默断开可能要到下一次远端调用时才会发现。`set_remote` 的设置仅在当前 proxy 进程内有效，地址变化时会丢弃旧会话，下一次远端调用再连接新地址。

## MCP 工具

| 工具 | 主要参数 | 说明 |
| --- | --- | --- |
| `read_text` | `path`, `offset?`, `max_bytes?` | 有界读取远端文本，最大 1 MiB。 |
| `tail_text` | `path`, `lines?`, `max_bytes?` | 有界读取文件尾，最多 10,000 行或 1 MiB。 |
| `write_text` | `path`, `content` | 原子写入远端文本。 |
| `ls` | `path`, `cursor?`, `limit?` | 排序并分页列出远端目录。 |
| `stat` | `path` | 不跟随符号链接读取元数据。 |
| `file_hash` | `path`, `max_bytes?` | 计算最大 64 MiB 文件的 SHA-256。 |
| `pids` | `filter?`, `cursor?`, `limit?` | Linux/Windows 进程分页；不可读取的 Windows 命令行返回空字符串。 |
| `process_info` | `pid` | Linux/Windows 进程详情；Windows 的 `state`、`uid` 返回 `null`。 |
| `kill` | `pid`, `signal?` | Unix 发送数字信号；Windows 接受 9/15 并强制终止进程。默认 15。 |
| `sh_exec` | `command`, `timeout_ms?` | 通过 `/bin/sh -c` 执行，最长 300 秒。 |
| `exec` | `program`, `args?`, `cwd?`, `env?`, `timeout_ms?` | 不经过 shell 执行程序。 |
| `system_info` | 无 | Linux/Windows 系统、运行时间、内存、系统盘和可用的负载/温度信息。 |
| `upload_file` | `local_path`, `remote_path`, `overwrite?` | 从 proxy 所在 PC 上传一个普通文件。 |
| `download_file` | `remote_path`, `local_path`, `overwrite?` | 下载一个普通文件到 proxy 所在 PC。 |
| `remote_status` | 无 | 查询当前 `ip`、`port`、`address` 和缓存的 `connected` 状态，不主动连接。 |
| `set_remote` | `ip?`, `port?` | 动态设置远端 IPv4 或端口；至少提供一项，未提供部分保持不变。 |

`overwrite` 默认为 `true`。传输工具拒绝目录、符号链接和特殊文件；目录传输可由 Agent 先通过 `exec`/`sh_exec` 打包，再传输生成的归档文件。

## 传输协议和安全边界

- TCP 握手使用双方随机 nonce 和内置值 `JARK006_PSK` 派生会话密钥。
- 帧头、请求 ID、序号和 payload 均受 HMAC 保护，用于避免本地网络中的误连接和传输损坏。
- 控制帧最大 2 MiB，二进制 chunk 固定上限 64 KiB。
- 此协议不加密内容。网络观察者仍可看到路径、命令和文件内容。
- 固定连接值不是安全凭据，无法隔离能够读取程序或源码的参与者。agent 提供远程 shell 级权限，只能部署在本地可信网络中，不要直接暴露到互联网。

## 验证

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --target x86_64-unknown-linux-musl
cargo check -p remote-ops-agent --target aarch64-unknown-linux-musl
cargo check -p remote-ops-agent --target armv7-unknown-linux-musleabi
```

测试包含认证失败、HMAC 标准向量、帧篡改与重放、MCP stdio 发现，以及跨多个 chunk 的 150,000 字节二进制上传/下载往返。
