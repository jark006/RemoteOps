# 🛰️ RemoteOps

RemoteOps 是一个面向远程系统维护和嵌入式 Linux 开发的 MCP 工具。
在 PC 端部署 `remote-ops-proxy` 并配置 MCP，远端设备部署 `remote-ops-agent`，则 PC 端的 Claude Code / Codex 即可远程控制远端设备。

```text
Claude Code / Codex
       v
   MCP stdio
       v
remote-ops-proxy
       ^
       |
       v
remote-ops-agent
       v
remote filesystem / process / shell
```

## 🚀 快速上手

到 [Release](https://github.com/jark006/RemoteOps/releases) 里下载最新的 PC 端 `remote-ops-proxy` 可执行文件，再丢到环境变量的某个目录里。再下载被控端的 `remote-ops-agent` 可执行文件，丢到开发板或需要被控制的系统。

⚠️ 这些可执行文件都带了目标平台的名称后缀，要么重命名将其移除，要么在下面配置的时候使用完整文件名。

## 🤖 启动被控端 agent

在被控端 Ubuntu 或 嵌入式 Linux 执行：

```sh
# 启动进程到后台 默认监听 0.0.0.0:8022
nohup ./remote-ops-agent > /dev/null 2>&1 &

# 也可以指定监听IP及端口
nohup ./remote-ops-agent --listen 0.0.0.0:8022 > /dev/null 2>&1 &
```

如果被控端是 Windows 则执行：

```powershell
.\remote-ops-agent.exe --listen 0.0.0.0:8022
```

## 🔧 配置 MCP proxy

通用 MCP 客户端配置示例如下，可以直接把以下内容丢给AI让他自己配置，然后重启 Claude Code 或 Codex 即可生效：

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
--remote IPv4:PORT           可选，默认 192.168.43.106:8022，也可随时在对话中叫 AI 设定 remote-ops 的受控端 IP
--timeout-ms N               等待远端操作响应的 I/O 超时，默认 310000
--max-transfer-bytes N       单文件上限，默认 4294967296（4 GiB）
```

## 💬 开始对话


> 用户： 使用 remote-ops 连接到 192.168.43.106 看看远端设备状况。

> 用户： 新增XX功能/优化XX相关逻辑/优化XX的性能，你要自行完成代码编辑、编译，通过 remote-ops 连接到 192.168.43.106 目标平台进行部署、运行及调试。


## 🛠️ MCP 工具

| 工具 | 主要参数 | 说明 |
| --- | --- | --- |
| `read_text` | `path`, `offset?`, `max_bytes?` | 有界读取远端文本，最大 1 MiB。 |
| `tail_text` | `path`, `lines?`, `max_bytes?` | 有界读取文件尾，最多 10,000 行或 1 MiB。 |
| `write_text` | `path`, `content` | 原子写入远端文本。 |
| `apply_patch` | `path`, `patch`, `expected_sha256?` | 对单个远端 UTF-8 文本文件原子应用上下文补丁。 |
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

`apply_patch` 只支持更新已存在的普通文件，`patch` 最大 256 KiB，目标文件最大 16 MiB，每次最多 128 个 hunk。补丁中的路径必须与 `path` 完全一致；每个旧文本片段必须唯一匹配，否则不修改文件。可传入当前文件的 `expected_sha256` 防止覆盖并发修改。格式如下：

```text
*** Begin Patch
*** Update File: /etc/example.conf
@@
-old value
+new value
 unchanged context
*** End Patch
```

补丁保留 UTF-8 BOM、原有行尾和末尾换行状态；新增行沿用文件现有的 LF 或 CRLF。首版不支持创建、删除或重命名文件，也不支持无上下文的纯插入 hunk。

⚠️ 被控端 Windows 的 `sh_exec` 固定使用 `C:\Program Files\Git\bin\bash.exe --noprofile --norc -c`，不搜索 PATH 或回退到其他 shell；该文件不存在或不是普通文件时返回 unsupported。

## 🔒 传输协议和安全边界

- 当前远端协议版本为 2；proxy 与 agent 版本不一致时握手失败，不进行兼容降级。
- TCP 握手使用双方随机 nonce 和内置值 `JARK006_PSK` 派生会话密钥。
- 帧头、请求 ID、序号和 payload 均受 HMAC 保护，用于避免本地网络中的误连接和传输损坏。
- 控制帧最大 2 MiB，二进制 chunk 固定上限 64 KiB。
- 此协议不加密内容。网络观察者仍可看到路径、命令和文件内容。
- 固定连接值不是安全凭据，无法隔离能够读取程序或源码的参与者。agent 提供远程 shell 级权限，只能部署在本地可信网络中，不要直接暴露到互联网。

---

## 🏗️ 构建

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
# 在 Win 端开发时安装 zig
winget install zig.zig

# 在 Ubuntu 端开发时安装 zig
sudo snap install zig --classic --beta

# 安装 zigbuild 及相关工具链
cargo install --locked cargo-zigbuild
rustup target add armv7-unknown-linux-musleabihf
rustup target add aarch64-unknown-linux-musl
rustup target add x86_64-unknown-linux-musl
rustup target add riscv64gc-unknown-linux-musl

# 交叉编译
cargo zigbuild --target armv7-unknown-linux-musleabihf --release
cargo zigbuild --target aarch64-unknown-linux-musl --release
cargo zigbuild --target x86_64-unknown-linux-musl --release
cargo zigbuild --target riscv64gc-unknown-linux-musl --release
```

## ✅ 验证

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --target x86_64-unknown-linux-musl
cargo check --target aarch64-unknown-linux-musl
cargo check --target armv7-unknown-linux-musleabi
```

测试包含认证失败、HMAC 标准向量、帧篡改与重放、MCP stdio 发现，以及跨多个 chunk 的 150,000 字节二进制上传/下载往返。
