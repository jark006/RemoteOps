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

查看版本：

```sh
./remote-ops-agent --version
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
--version / -v               打印版本及项目地址后退出
```

## 💬 开始对话


> 用户： 使用 remote-ops 连接到 192.168.43.106 看看远端设备状况。

> 用户： 新增XX功能/优化XX相关逻辑/优化XX的性能，你要自行完成代码编辑、编译，通过 remote-ops 连接到 192.168.43.106 目标平台进行部署、运行及调试。


## 🛠️ MCP 工具

proxy 当前暴露 38 个 MCP 工具：

| 工具 | 主要参数 | 说明 |
| --- | --- | --- |
| `read_text` | `path`, `offset?`, `max_bytes?` | 有界读取远端文本，最大 1 MiB。 |
| `read_file_lines` | `path`, `start_line?`, `end_line?`, `max_bytes?` | 按 1-based inclusive 行号读取 UTF-8 文本；默认从第 1 行读取 200 行，最多 10,000 行或返回 1 MiB。 |
| `tail_text` | `path`, `lines?`, `max_bytes?` | 有界读取文件尾，最多 10,000 行或 1 MiB。 |
| `write_text` | `path`, `content` | 原子写入远端文本。 |
| `apply_patch` | `path`, `patch`, `expected_sha256?` | 对单个远端 UTF-8 文本文件原子应用上下文补丁。 |
| `list_files` | `path`, `cursor?`, `limit?`, `recursive?`, `pattern?`, `max_depth?` | 排序、过滤并分页列出远端目录；可递归返回相对路径。每个条目返回 `name`、`kind`、`size`、`mtime`、`mtime_iso`（RFC 3339、Agent 本地时区）和 `mode_str`（`ls -l` 风格权限串）。 |
| `grep` | `path`, `pattern`, `glob?`, `case_sensitive?`, `max_results?`, `max_file_bytes?` | 对远端普通 UTF-8 文件执行有界逐行 Rust 正则搜索。 |
| `stat` | `path` | 不跟随符号链接读取元数据；返回 `size`、`mtime`、`mtime_iso`（RFC 3339、Agent 本地时区）、`mode`、`mode_str`（`ls -l` 风格权限串）和 `kind`。 |
| `file_hash` | `path`, `max_bytes?` | 计算最大 64 MiB 文件的 SHA-256。 |
| `mkdir` | `path`, `recursive?`, `mode?` | 创建精确指定的远端目录；可显式递归创建父目录并设置 Unix mode。 |
| `remove` | `path`, `recursive?` | 删除一个精确路径；目录默认只允许空目录，递归删除必须显式开启。 |
| `move` | `source`, `destination`, `overwrite?` | 同一文件系统内移动文件、目录或符号链接；跨文件系统返回结构化错误。 |
| `copy` | `source`, `destination`, `overwrite?`, `recursive?` | 复制普通文件或显式递归复制目录，不跟随符号链接。 |
| `chmod` | `path`, `mode` | 在 Unix 上设置普通文件或目录 mode，不跟随符号链接。 |
| `symlink` | `target`, `link_path`, `overwrite?`, `target_kind?` | 创建一个符号链接；Windows 必须提供 `target_kind`。 |
| `sync_directory` | `local_path`, `remote_path`, `excludes?`, `max_files?`, `max_total_bytes?`, `max_depth?` | 生成 manifest，只上传变化文件，在远端 staging 完整校验后切换目录并保留旧树。 |
| `deploy_release` | `local_path`, `releases_path`, `current_path`, `release_id`, `start`, `health`, ... | Unix 发布事务：preflight、同步独立 release、原子切换 symlink、启动和健康检查，失败自动回滚。 |
| `pids` | `filter?`, `cursor?`, `limit?` | Linux/Windows/macOS 进程分页；不可读取的 Windows/macOS 命令行返回空字符串。 |
| `process_info` | `pid` | Linux/Windows/macOS 进程详情；Windows 的 `state`、`uid` 返回 `null`。`start_time_seconds` 为开机后秒数，`start_time_iso` 为进程启动墙钟时间（RFC 3339、Agent 本地时区，Linux 无法确定时为 `null`）。 |
| `kill` | `pid`, `signal?` | Unix 发送数字信号；Windows 接受 9/15 并强制终止进程。默认 15。 |
| `pkill` | `name`, `signal?` | 按平台进程名完整匹配（Windows 不区分大小写）并排除 agent 自身，默认 signal 15；Linux/macOS 名称分别最多 15/31 字节，Windows 最多 260 个 UTF-16 单元且 signal 仅接受 9/15。匹配超过 1024 个进程时不执行，返回 `matched`、`signaled_pids` 和 `failed_pids`。 |
| `sh_exec` | `command`, `timeout_ms?` | Unix 通过 `/bin/sh -c` 执行；Windows 通过固定路径 Git Bash 执行，不存在时返回 unsupported。最长 300 秒。结果含 `duration_ms`。 |
| `exec` | `program`, `args?`, `cwd?`, `env?`, `timeout_ms?` | 不经过 shell 执行程序。结果含 `duration_ms`。 |
| `process_start` | `program`, `args?`, `cwd?`, `env?`, `timeout_ms?` | 启动由 agent 管理的后台程序并立即返回 job ID；默认最长 1 小时，最大 24 小时。 |
| `process_output` | `job_id`, `stdout_cursor?`, `stderr_cursor?`, `max_bytes?` | 按绝对字节游标增量读取后台任务的 stdout/stderr。 |
| `process_wait` | `job_id`, `wait_ms?` | 有界等待后台任务退出，默认 10 秒，最长 30 秒。 |
| `process_signal` | `job_id`, `signal?` | 向 Unix 任务进程组发送信号；Windows 接受 9/15 并终止整个 Job Object。 |
| `process_close` | `job_id` | 释放已结束任务及其保留输出；运行中的任务必须先 signal 并 wait。 |
| `system_info` | 无 | 有界读取系统、CPU、身份权限、网络、文件系统、时间（含 RFC 3339 的 `time.iso`）、init 和工具链画像。 |
| `upload_file` | `local_path`, `remote_path`, `overwrite?`, `mode?`, `resume?` | 从 proxy 所在 PC 上传一个普通文件，可设置 Unix mode 并校验续传。 |
| `download_file` | `remote_path`, `local_path`, `overwrite?`, `resume?` | 下载一个普通文件到 proxy 所在 PC，可校验续传。 |
| `remote_status` | 无 | 被动查询地址、缓存连接、生命周期状态，以及最近一次成功、错误、探测和 Agent 信息，不主动连接。 |
| `set_remote` | `ip?`, `port?` | 动态设置远端 IPv4 或端口；至少提供一项，未提供部分保持不变。 |
| `agent_info` | 无 | 查询 Agent 版本、协议、构建信息、运行实例、平台、能力和限制。 |
| `remote_probe` | `timeout_ms?` | 主动连接或健康检查远端，返回可达性、延迟、Agent 信息或结构化错误。 |
| `wait_remote` | `wait_for?`, `timeout_ms?`, `poll_interval_ms?`, `probe_timeout_ms?` | 有界轮询远端，等待 `online`、`offline` 或 `offline_then_online`。 |
| `reboot` | `delay_ms?` | 请求 Agent 延迟重启设备，并将 proxy 生命周期状态切换为 `rebooting`。 |
| `agent_update` | `local_path`, `timeout_ms?`, `poll_interval_ms?`, `probe_timeout_ms?` | 上传并验证 Agent 候选程序，原子替换、重启验证，失败时自动回滚。 |
| `batch` | `calls` | 在一次往返内按顺序执行至多 16 个只读与诊断工具（含 `sh_exec`/`exec`），子结果按输入顺序返回 `{tool, ok, result\|error}`；写操作整体拒绝。纯 proxy 实现，旧 Agent 无需升级。 |

`upload_file` 和 `download_file` 的 `overwrite` 默认为 `true`，`resume` 默认为 `false`。续传开启后，接收端保留同目录 partial 文件，双方先校验已传前缀的 SHA-256，再从确认的字节偏移继续；远端内容变化导致下载前缀不匹配时会安全回退到偏移 0。最终仍校验完整长度和 SHA-256，成功前不会暴露半文件。`upload_file.mode` 范围为 `0..=0o7777`，只在 Unix Agent 上支持。单文件传输拒绝目录、符号链接和特殊文件。

`mkdir`、`remove`、`move`、`copy`、`chmod` 和 `symlink` 都要求精确路径。`remove` 的递归模式和 `copy` 的目录递归模式最多处理 100,000 个条目，先拒绝特殊文件；递归复制不接受符号链接。`move` 不隐式退化为 copy-delete，遇到跨文件系统移动返回 `cross_filesystem`。覆盖目录始终被拒绝，覆盖非目录必须显式设置 `overwrite`。

`sync_directory` 在 proxy 所在 PC 扫描普通文件和目录，拒绝符号链接、特殊文件及非 UTF-8 相对路径。manifest 默认最多 4,096 个条目、4 GiB、32 层，硬上限分别为 10,000 个、4 GiB、64 层；最多 64 个排除 glob，每个最多 1 KiB。Agent 将远端未变化文件复制进 staging，只传输大小或 SHA-256 不同的文件，并在远端支持时保留文件、目录及根目录 Unix mode；远端不支持（如 Windows Agent）时，proxy 会在 manifest 中省略 mode 并正常同步。提交前逐项复核 staging，随后将旧目标改名为返回的 `backup_path`，不会静默删除备份。

`deploy_release` 仅在 Unix Agent 上支持。`release_id` 只能使用 ASCII 字母、数字、`.`、`_` 和 `-`；新版本同步到 `releases_path/release_id`。preflight 检查远端架构、所需磁盘空间、release/current 父目录写权限和最多 64 个依赖程序。`stop`、`start`、`health`、`rollback_start` 均采用与 `exec` 相同的不经过 shell 的结构化命令，单步最长 300 秒；`start` 和 `health` 必填。Agent 在同一请求内停止服务、原子替换 `current_path` symlink、启动并执行健康检查，启动或健康检查失败时切回旧 symlink，并按 `rollback_start`（默认复用 `start`）恢复旧服务。

`system_info` 保留原有的主机、内核、运行时间、负载、内存、系统盘和温度字段，并新增 `os`、`cpu`、`identity`、`network`、`filesystems`、`time`、`init_system` 和 `toolchains`。Linux 会解析 `/etc/os-release`、CPU 拓扑和 libc/ABI，当前用户、组、umask 与 capabilities，网卡/IP、IPv4/IPv6 路由、DNS、TCP/UDP 监听端口，以及 mount 类型、空间、inode 和只读状态。网卡最多 128 个、地址最多 512 个、路由最多 256 条、监听端口最多 512 个、mount 最多 256 个、工具链最多 24 个；各集合通过 `available` 和 `truncated` 区分平台不可采集与结果截断。Windows/macOS 返回可可靠采集的同构字段，Linux 专属信息明确标记为不可用，不伪造数据。

后台任务最多同时保留 16 个。`process_start` 不经过 shell，stdin 固定为空；需要 shell 语法时可将 `program` 设为 `/bin/sh`，并使用 `args: ["-c", "..."]`。每个任务的 stdout 和 stderr 各保留最近 256 KiB，`process_output.max_bytes` 默认每路 64 KiB、最大每路 256 KiB；输出以有损 UTF-8 字符串返回，游标按原始字节计数。请求游标早于仍保留的内容时，对应 `*_truncated` 为 `true`，并从 `*_start_cursor` 继续。

任务属于 agent 进程而不是单个 TCP 会话，proxy 断线重连后仍可凭 `job_id` 查询。任务不会跨 agent 进程重启持久化。达到 16 个任务时会先回收最早结束的任务；如果 16 个任务都仍在运行，新的 `process_start` 会被拒绝。`process_close` 可用于及时释放已结束任务。

`read_file_lines` 未提供 `end_line` 时读取从 `start_line` 开始的 200 行；为定位行号最多扫描 64 MiB。达到 `max_bytes` 时不会返回半行，`next_line` 指向下一次应读取的行。

`list_files` 的 `limit` 默认 200、最大 1,000，`recursive` 默认 `false`，`max_depth` 默认 16、最大 64。递归结果的 `name` 使用相对请求目录的 `/` 分隔路径；`pattern` 是最大 1 KiB 的 glob。符号链接会列出但不会遍历，单次最多扫描 100,000 个目录项并输出 1 MiB。

`grep` 的正则最大 4 KiB，结果默认 200 条、最多 1,000 条；`case_sensitive` 默认 `true`，`max_file_bytes` 默认 1 MiB、最大 16 MiB。单次最多枚举 100,000 个目录项、递归 64 层、扫描 10,000 个文件或 64 MiB 并输出 1 MiB，单条匹配文本最多 1 KiB。目录搜索不跟随符号链接，并跳过 `.git`、`.hg`、`.svn`、`.next`、`node_modules`、`target`、`dist`、`build`；`glob` 最大 1 KiB，匹配相对路径。

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

### Agent 与设备生命周期

- Agent 同一时刻只允许一个已认证 proxy 作为活动管理者。新的 proxy 完成认证后会接管空闲管理连接，并由 Agent 关闭旧连接，因此 Codex 使用 `/clear` 创建新会话后，新会话的 proxy 可以直接继续管理设备；旧 proxy 进程可能仍由 Codex 保留，但不再占用远端连接。如果当前管理者正在执行请求、传输或提交文件，新候选的首个请求会收到 `manager_busy` 且保证未执行；认证失败不会影响当前管理者。
- `remote_status` 是被动快照，不会为确认设备在线而建立连接。`connection_state` 为 `cached` 或 `disconnected`，只表示 proxy 是否持有已认证会话；`lifecycle_state` 为 `ready`、`rebooting` 或 `updating`。结果还包含 `last_success_at_ms`、`last_error`、`last_probe` 和最近缓存的 `agent_info`。
- `remote_probe` 会主动建立连接或复用缓存连接执行健康检查。`timeout_ms` 默认 5,000 ms，范围 100..=30,000 ms；结果始终给出 `reachable`、延迟、是否复用连接、生命周期状态，以及 Agent 信息或结构化错误。
- `wait_remote` 支持 `online`、`offline` 和 `offline_then_online`。正常状态默认等待 `online`，重启或更新期间默认等待 `offline_then_online`；后者在观察到离线后重新在线，或 Agent `instance_id` 已变化时完成。总超时默认 120,000 ms、范围 1..=600,000 ms，轮询间隔默认 1,000 ms、范围 100..=10,000 ms，每次探测超时默认 5,000 ms、范围 100..=30,000 ms。
- `reboot` 的延迟默认 1,000 ms，范围 250..=10,000 ms。Agent 先确认请求再延迟执行，proxy 随后丢弃缓存会话并进入 `rebooting`；如果响应恰好因重启断开而丢失，结果会通过 `acknowledged` 和 `disconnect_observed` 区分。可继续使用 `wait_remote` 等待设备恢复。普通请求在发送后发生连接错误时仍不会自动重放。
- `agent_update` 的 `local_path` 指向 proxy 所在 PC 上的候选 Agent 普通文件。proxy 先探测现有 Agent，将候选文件上传到 Agent 公布的固定 staging 路径并校验 SHA-256；候选程序通过 `--self-check` 校验协议版本和构建 target 后，由独立 helper 等待旧 Agent 退出、原子替换程序并重启。新 Agent 成功监听且通过稳定性检查后删除备份；启动失败则恢复旧程序并重启。结果以 `updated`、`rolled_back`、`timed_out` 或 `unconfirmed` 明确报告状态。等待和探测参数范围与 `wait_remote` 相同。
- Linux 的 `reboot` 调用系统 `reboot(2)`，Agent 必须以 root 运行；Windows 使用 `shutdown.exe /r /t 0 /f`；macOS 返回结构化 `unsupported`。`agent_info` 会据实报告 `reboot` 和 `self_update` 能力。

⚠️ 被控端 Windows 的 `sh_exec` 固定使用 `C:\Program Files\Git\bin\bash.exe --noprofile --norc -c`，不搜索 PATH 或回退到其他 shell；该文件不存在或不是普通文件时返回 unsupported。

## 🔒 传输协议和安全边界

- 当前远端协议版本为 3；proxy 与 agent 版本不一致时握手失败，不进行兼容降级。v3 增加续传偏移/前缀哈希、上传 mode、目录 manifest 和部署事务消息。
- TCP 握手使用双方随机 nonce 和内置值 `JARK006_PSK` 派生会话密钥。
- 帧头、请求 ID、序号和 payload 均受 HMAC 保护，用于避免本地网络中的误连接和传输损坏。
- 控制帧最大 2 MiB，二进制 chunk 固定上限 64 KiB。
- 此协议不加密内容。网络观察者仍可看到路径、命令和文件内容。
- 固定连接值不是安全凭据，无法隔离能够读取程序或源码的参与者。agent 提供远程 shell 级权限，只能部署在本地可信网络中，不要直接暴露到互联网。

---

## 🏗️ 构建

构建 Win(x64/arm64)、Mac(x64/arm64)、Linux(x64/arm64/arm32/riscv64gc/MIPS32r2) 共 9 个目标平台

### 1. 安装 RUST 开发环境

https://rust-lang.org/zh-CN/learn/get-started/

### 2. 安装 zig 以支持交叉编译

```sh
# 在 Win 端开发
winget install zig.zig

# 在 Mac 端开发
brew install zig

# 在 Linux 端开发 (根据发行版选择)
sudo snap install zig --classic --beta
pacman -S zig
dnf install zig
```

### 3. 安装 zigbuild 及相关工具链

```sh
cargo install --locked cargo-zigbuild
rustup target add aarch64-apple-darwin
rustup target add x86_64-apple-darwin
rustup target add x86_64-pc-windows-gnullvm
rustup target add aarch64-pc-windows-gnullvm
rustup target add armv7-unknown-linux-musleabihf
rustup target add aarch64-unknown-linux-musl
rustup target add x86_64-unknown-linux-musl
rustup target add riscv64gc-unknown-linux-musl
rustup target add loongarch64-unknown-linux-musl
# MIPS32r2 是 Rust Tier 3 目标，需要从源码构建标准库。
rustup toolchain install nightly --component rust-src
```

### 4. 编译
```sh
cargo zigbuild --release --target aarch64-apple-darwin
cargo zigbuild --release --target x86_64-apple-darwin
cargo zigbuild --release --target x86_64-pc-windows-gnullvm
cargo zigbuild --release --target aarch64-pc-windows-gnullvm
cargo zigbuild --release --target armv7-unknown-linux-musleabihf
cargo zigbuild --release --target aarch64-unknown-linux-musl
cargo zigbuild --release --target x86_64-unknown-linux-musl
cargo zigbuild --release --target riscv64gc-unknown-linux-musl
cargo zigbuild --release --target loongarch64-unknown-linux-musl

# Ingenic XBurst / MIPS32r2：仅构建被控端 Agent。
# 目标规格固定为 little-endian、o32、MIPS32r2、soft-float，并由 Zig 静态链接 musl。
cargo +nightly build -p remote-ops-agent --release --target targets/mipsel-unknown-linux-musl.json -Z json-target-spec -Z build-std=std,panic_abort
```

Agent 的 `build.rs` 会在编译时注入 target、profile 和 Git revision；这些信息可通过 `agent_info.build` 查看，并用于自更新候选程序的兼容性检查。可设置 `REMOTE_OPS_GIT_REVISION` 显式指定 revision，否则构建脚本尝试读取当前 Git 提交，无法读取时使用 `unknown`。

工作区的 `[profile.release]` 已针对 flash 资源受限平台（如 armv7）做了体积优化，且以性能优先为前提：开启全程序 LTO、单 codegen unit、`opt-level = "s"`（体积感知，保留内联/向量化等主要优化）并剥离符号表；SHA-256 与正则匹配路径单独保持最高优化级别，确保大文件传输和 `grep` 不受影响。

### Ingenic XBurst MIPS32r2 Linux 3.10.14

`targets/mipsel-unknown-linux-musl.json` 产出的 Agent 位于 `target/mipsel-unknown-linux-musl/release/remote-ops-agent`。它是静态链接的 little-endian MIPS32r2/o32/soft-float ELF，不依赖设备固件中的 glibc 或 uClibc；不要对该目标使用 `cargo zigbuild`，因为 Zig 的 ABI 名称为 `mipsel-linux-musleabi`，由目标规格直接传给 Zig。


## ✅ 验证

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --target x86_64-unknown-linux-musl
cargo check -p remote-ops-agent --target aarch64-unknown-linux-musl
cargo check -p remote-ops-agent --target armv7-unknown-linux-musleabihf
cargo +nightly build -p remote-ops-agent --release --target targets/mipsel-unknown-linux-musl.json -Z json-target-spec -Z build-std=std,panic_abort
```

测试包含认证失败、协议版本拒绝、HMAC 标准向量、帧篡改与重放、MCP stdio 发现、后台任务增量输出与断线重连、Agent 生命周期与更新校验、跨多个 chunk 的二进制往返、前缀校验续传、只传变化文件的目录同步，以及 Unix 发布切换和健康检查失败回滚。
