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
- proxy 默认使用 `192.168.43.106:8022`，AI Agent 可查询连接状态并在运行时切换远端 IPv4 或端口。
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
| `sh_exec` | 支持 | 支持⚠️ | 支持 |
| `kill` | 支持 | 支持 | 支持 |
| `pkill` | 支持 | 支持 | 支持 |
| `pids` | 支持 | 支持 | 支持 |
| `process_info` | 支持 | 支持 | 支持 |
| `system_info` | 支持 | 支持 | 支持 |

Windows 的 `sh_exec` 固定使用 `C:\Program Files\Git\bin\bash.exe --noprofile --norc -c`，不搜索 PATH 或回退到其他 shell；该文件不存在或不是普通文件时返回 unsupported。

## 构建

```sh
cargo build --release --workspace
```

本机产物位于：

```text
target/release/remote-ops-proxy
target/release/remote-ops-agent
```

可使用 `cargo-zigbuild` 针对 Linux 平台进行交叉编译：

```sh
# 安装 zigbuild
winget install zig.zig
cargo install --locked cargo-zigbuild
rustup target add armv7-unknown-linux-musleabi
rustup target add aarch64-unknown-linux-musl
rustup target add x86_64-unknown-linux-musl
rustup target add riscv64gc-unknown-linux-musl

cargo zigbuild --target armv7-unknown-linux-musleabi --release
cargo zigbuild --target aarch64-unknown-linux-musl --release
cargo zigbuild --target x86_64-unknown-linux-musl --release
cargo zigbuild --target riscv64gc-unknown-linux-musl --release
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
--max-transfer-bytes N       单文件上限，默认 4294967296（4 GiB）
```

agent 串行服务一条 proxy 连接。认证后的连接不会因为空闲而关闭；连接断开后会继续等待下一次连接。为防止无效连接永久占用唯一服务槽，认证握手限时 10 秒，一个已经开始的帧必须在 30 秒内完成，上传相邻帧最多间隔 60 秒，socket 写入限时 30 秒。agent 和 proxy 都启用 TCP Keepalive，空闲 60 秒后每 10 秒探测一次，失败重试次数采用平台默认值。

## 配置 MCP proxy

Claude Code、Codex 等 AI Agent 通过 stdio 与 remote-ops-proxy 通信，而 remote-ops-proxy 通过网络与远程 remote-ops-agent 通信。通用 MCP 客户端配置示例：

Claude Code: ~/.claude.json
```json
{
  "mcpServers": {
    "remote-ops": {
      "type": "stdio",
      "command": "remote-ops-proxy",
      "args": []
    }
  }
}
```

Codex: ~/.codex/config.toml
```toml
[mcp_servers]
[mcp_servers.remote-ops]
type = "stdio"
command = "remote-ops-proxy"
args = []
```

proxy 参数：

```text
--remote IPv4:PORT           可选，默认 192.168.43.106:8022
--timeout-ms N               等待远端操作响应的 I/O 超时，默认 310000
--max-transfer-bytes N       单文件上限，默认 4294967296（4 GiB）
```

proxy 会在首次远端工具调用时建立连接，因此 `initialize` 和 `tools/list` 不要求远端在线。TCP 连接和认证握手分别限时 10 秒。缓存连接空闲超过 60 秒后，proxy 会在发送用户请求前执行一次 10 秒超时的内部健康检查；检查失败时先丢弃旧会话并重新连接。健康检查复用现有 Request/Response，不属于 MCP 工具。用户请求一旦开始发送，连接失败只会返回状态不确定错误，绝不会自动重放。

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
| `pids` | `filter?`, `cursor?`, `limit?` | Linux/Windows/macOS 进程分页；不可读取的 Windows/macOS 命令行返回空字符串。 |
| `process_info` | `pid` | Linux/Windows/macOS 进程详情；Windows 的 `state`、`uid` 返回 `null`。 |
| `kill` | `pid`, `signal?` | Unix 发送数字信号；Windows 接受 9/15 并强制终止进程。默认 15。 |
| `pkill` | `name`, `signal?` | 按平台进程名完整匹配并排除 agent 自身，默认 signal 15；Linux/macOS 名称分别最多 15/31 字节，Windows 最多 260 个 UTF-16 单元且 signal 仅接受 9/15。匹配超过 1024 个进程时不执行，返回 `matched`、`signaled_pids` 和 `failed_pids`。 |
| `sh_exec` | `command`, `timeout_ms?` | Unix 通过 `/bin/sh -c` 执行；Windows 通过固定路径 Git Bash 执行，不存在时返回 unsupported。最长 300 秒。 |
| `exec` | `program`, `args?`, `cwd?`, `env?`, `timeout_ms?` | 不经过 shell 执行程序。 |
| `system_info` | 无 | 运行时间、内存、系统盘和可用的负载/温度信息。 |
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
cargo check --target aarch64-unknown-linux-musl
cargo check --target armv7-unknown-linux-musleabi
```

测试包含认证失败、HMAC 标准向量、帧篡改与重放、MCP stdio 发现，以及跨多个 chunk 的 150,000 字节二进制上传/下载往返。
