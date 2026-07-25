# RemoteOps 项目约束

本文件为 Claude Code、Codex 等 AI Agent 工具提供项目约束。
需保证 AGENTS.md 和 CLAUDE.md 内容完全一致，对其中一个文件的内容修改，都要完全同步到另一个文件。

## 项目结构

- `crates/protocol`：proxy 与 agent 共享的认证、分帧和 wire 类型。
- `crates/proxy`：PC 端 MCP stdio server、本地文件 I/O 和远端连接客户端。
- `crates/agent`：远端监听服务、文件传输落盘和平台工具实现。
- `crates/proxy/tests/end_to_end.rs`：真实双端连接、二进制往返和 stdio 测试。

## 架构边界

- MCP JSON-RPC 只存在于 proxy 的 stdio 一侧，二进制文件内容不得编码进 MCP JSON。
- proxy 主动连接一个远端 agent；首版协议串行处理请求，不支持复用或自动重放。
- 认证后的连接不得因为空闲而关闭。握手、已开始帧、传输帧间隔和 socket 写入分别保持有界超时，并启用 TCP Keepalive；proxy 可在用户请求发送前通过内部 Request/Response 健康检查安全重连。
- 控制消息使用 JSON，文件内容使用 `Chunk` 帧。修改 wire 类型或帧格式时必须提升协议版本并补兼容/拒绝测试。
- proxy 和 agent 固定使用 `BUILTIN_PSK = b"JARK006_PSK"`，不得要求部署者配置 `REMOTE_OPS_PSK`。帧完整性与防重放保护仍须保留；协议不承诺认证安全或机密性。
- stdout 是 proxy 的 MCP 协议通道，任何日志只能写 stderr。

## 工具契约

proxy 暴露 20 个工具。修改名称、参数、默认值、上限或结果字段时，必须同步更新 schema、测试、README 和本文件。

- 兼容工具：`read_text`、`read_file_lines`、`tail_text`、`write_text`、`apply_patch`、`list_files`、`grep`、`stat`、`file_hash`、`pids`、`process_info`、`kill`、`sh_exec`、`exec`、`system_info`。
- 进程工具：`pkill`。
- 传输工具：`upload_file`、`download_file`。
- proxy 本地管理工具：`remote_status`、`set_remote`。前者被动查询当前地址和缓存连接状态；后者可单独设置 IPv4 或端口，配置仅在当前进程内生效。
- `upload_file` 的 `local_path` 和 `download_file` 的 `local_path` 均属于 proxy 所在 PC。
- 单文件默认上限 4 GiB，chunk 上限 64 KiB；控制帧上限 2 MiB。
- `apply_patch` 仅更新一个已存在的普通 UTF-8 文本文件，补丁路径必须与请求路径完全一致；补丁最大 256 KiB，目标文件最大 16 MiB，最多 128 个 hunk，不支持创建、删除、重命名或无上下文纯插入。旧侧上下文必须唯一匹配，可用 `expected_sha256` 检测冲突；整个补丁成功前不得修改目标，并保留 BOM、原有行尾和末尾换行状态。
- `read_file_lines` 使用 1-based inclusive 行号，`start_line` 默认 1，未提供 `end_line` 时默认读取 200 行；单次最多 10,000 行、返回 1 MiB，并将为定位起始行而扫描的内容限制为 64 MiB。只接受非符号链接的普通文件，请求范围必须是 UTF-8。
- `list_files` 对目录项排序并分页，`limit` 默认 200、最大 1,000；`recursive` 默认 false，递归时 `name` 为相对请求目录的 `/` 分隔路径，`max_depth` 默认 16、最大 64。可用最大 1 KiB 的 `pattern` glob 过滤相对路径；符号链接可列出但不遍历，单次最多扫描 100,000 个目录项、输出 1 MiB。
- `grep` 对单个普通文件或目录树中的普通 UTF-8 文件执行逐行 Rust 正则搜索，大小写敏感默认开启，可用最大 1 KiB 的 `glob` 过滤相对路径。正则最大 4 KiB，结果默认 200 条、最多 1,000 条，单文件默认扫描 1 MiB、最多 16 MiB，单次总计最多枚举 100,000 个目录项、递归 64 层、扫描 10,000 个文件或 64 MiB、输出 1 MiB；匹配文本最多保留 1 KiB。目录搜索不跟随符号链接，并跳过 `.git`、`.hg`、`.svn`、`.next`、`node_modules`、`target`、`dist`、`build`。
- 文件传输必须校验长度和 SHA-256，成功前使用同目录临时文件，失败时不得留下目标半文件。
- 工具输出必须有界。命令 stdout/stderr 各限制为 256 KiB，命令超时最大 300 秒。

## 平台行为

- Linux 提供全部工具。
- Windows/macOS 提供通用文件工具、文件传输和 `exec`；Windows 还提供 `kill` 和 `system_info`。
- macOS/Unix 可提供 `sh_exec`、`kill` 和 `system_info`；Windows 的 `sh_exec` 固定使用 `C:\Program Files\Git\bin\bash.exe --noprofile --norc -c`，不搜索 PATH 或回退到其他 shell，Git Bash 不存在时返回结构化 unsupported。Windows 的 `kill` 仅接受 signal 9 或 15，两者均强制终止进程。
- Windows 的 `exec` 使用 Job Object 管理命令进程树，超时必须终止命令及其后代，不得遗留持有输出管道的子进程。
- `pids`、`process_info` 支持 Linux、Windows 和 macOS；macOS 使用原生 libproc/sysctl 接口，无法读取的命令行返回空字符串。
- `pkill` 支持 Linux、Windows 和 macOS，按平台进程名完整匹配并排除 agent 自身 PID；Linux `/proc/<pid>/comm` 名称最多 15 字节，macOS `pbi_name` 名称最多 31 字节，Windows 快照名称最多 260 个 UTF-16 单元。默认 signal 15，Windows 仅接受 9 或 15；最多匹配 1024 个目标，超过上限时不得发送任何信号。
- Windows 进程枚举遇到不可读取的进程时保留 PID 和可用字段，命令行返回空字符串；`process_info` 的 `state`、`uid` 返回 `null`。
- Windows `system_info` 返回主机、系统版本、运行时间、内存和系统盘信息；无 Unix load average 和统一温度接口，分别返回零值和空数组。macOS `system_info` 返回主机、内核、运行时间、内存和系统盘信息，并通过 `getloadavg` 提供真实负载；无统一温度接口，温度返回空数组。
- 不得为不支持的平台伪造空系统信息；应返回结构化错误。

## 修改要求

- 对 MCP 参数和远端参数进行双重校验，不信任任一端输入。
- 连接错误发生在请求发出后时，返回状态不确定错误并丢弃会话，禁止自动重放。
- 保持普通文件限制，明确拒绝目录、符号链接和特殊文件传输。
- Unix 覆盖写尽量保留既有 mode，并在原子替换后同步父目录。
- Windows 覆盖使用 `MoveFileExW` 的 replace/write-through 语义，不先删除目标文件。
- 避免引入 async runtime；当前单连接同步模型是首版有意的复杂度边界。

## 验证命令

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check --workspace --target x86_64-unknown-linux-musl
cargo check -p remote-ops-agent --target aarch64-unknown-linux-musl
cargo check -p remote-ops-agent --target armv7-unknown-linux-musleabihf
```

涉及发布产物时，再执行对应目标的 release build 或 `cargo zigbuild`。
