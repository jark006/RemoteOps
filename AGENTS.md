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
- proxy 主动连接一个远端 agent；Agent 同一时刻只允许一个已认证 proxy 作为活动管理者，首版协议串行处理请求，不支持复用或自动重放。
- 新 proxy 完成认证后可原子接管空闲活动管理连接，Agent 必须关闭被取代的旧连接；认证失败不得影响当前管理者。活动管理者从确认请求到完整响应发送期间均为 busy，此时新候选的首个请求必须返回 `manager_busy` 并保证未执行，不得中断或排队旧请求。候选连接和 handler 数量必须有界。
- 认证后的活动连接不得因为空闲而关闭；内部所有权轮询不得改变该语义。握手、已开始帧、传输帧间隔和 socket 写入分别保持有界超时，并启用 TCP Keepalive；proxy 可在用户请求发送前通过内部 Request/Response 健康检查安全重连。
- 控制消息使用 JSON，文件内容使用 `Chunk` 帧。修改 wire 类型或帧格式时必须提升协议版本并补兼容/拒绝测试。
- proxy 和 agent 固定使用 `BUILTIN_PSK = b"JARK006_PSK"`，不得要求部署者配置 `REMOTE_OPS_PSK`。帧完整性与防重放保护仍须保留；协议不承诺认证安全或机密性。
- stdout 是 proxy 的 MCP 协议通道，任何日志只能写 stderr。

## 工具契约

proxy 暴露 38 个工具。修改名称、参数、默认值、上限或结果字段时，必须同步更新 schema、测试、README 和本文件。

- 兼容工具：`read_text`、`read_file_lines`、`tail_text`、`write_text`、`apply_patch`、`list_files`、`grep`、`stat`、`file_hash`、`pids`、`process_info`、`kill`、`sh_exec`、`exec`、`system_info`。
- 进程工具：`pkill`。
- 后台任务工具：`process_start`、`process_output`、`process_wait`、`process_signal`、`process_close`。
- 传输工具：`upload_file`、`download_file`。
- 基础文件变更工具：`mkdir`、`remove`、`move`、`copy`、`chmod`、`symlink`。
- 目录同步与发布工具：`sync_directory`、`deploy_release`。
- Agent 生命周期工具：`agent_info`、`reboot`。
- proxy 本地管理与生命周期编排工具：`remote_status`、`set_remote`、`remote_probe`、`wait_remote`、`agent_update`。`remote_status` 被动查询地址、缓存连接和生命周期状态；`set_remote` 可单独设置 IPv4 或端口，配置仅在当前进程内生效；其余工具负责主动探测、等待状态变化和编排 Agent 自更新。
- `upload_file`、`download_file`、`sync_directory` 和 `deploy_release` 的 `local_path` 均属于 proxy 所在 PC。
- 单文件默认上限 4 GiB，chunk 上限 64 KiB；控制帧上限 2 MiB。
- 远端协议版本为 3。上传请求必须携带完整 SHA-256，可选 `mode` 范围为 `0..=0o7777` 且只在 Unix Agent 生效；上传和下载的 `resume` 默认 false。续传必须校验 partial 文件的已有长度和前缀 SHA-256，最终仍校验完整长度和 SHA-256；前缀不匹配的下载安全回退到偏移 0。成功前使用同目录 partial 文件，失败时仅在启用续传时保留可恢复 partial，目标不得出现半文件。
- `mkdir`、`remove`、`move`、`copy`、`chmod`、`symlink` 必须使用精确路径并区分普通文件、目录、符号链接和特殊文件。递归删除和复制最多检查 100,000 个条目；递归复制拒绝符号链接和特殊文件，递归删除不跟随符号链接并拒绝特殊文件。`move` 不得在跨文件系统时隐式 copy-delete；目录覆盖始终拒绝，非目录覆盖必须显式开启。
- `sync_directory` 基于排序 manifest、大小和 SHA-256，只传变化普通文件；本地符号链接、特殊文件和非 UTF-8 相对路径必须拒绝。同步在目标同级 staging 构建，提交前逐项复核类型、大小和哈希，再将旧目标改名为 `backup_path` 并切换 staging；旧 backup 不得静默删除。远端 Agent 支持时保留文件、目录和同步根目录 Unix mode，不支持时 proxy 在 manifest 中省略 mode 并正常同步。manifest 默认最多 4,096 个条目、4 GiB、32 层，最大 10,000 个条目、4 GiB、64 层；排除 glob 最多 64 个、每个 1 KiB。
- `deploy_release` 仅在 Unix Agent 上支持。`release_id` 限 1..=128 个 ASCII 字母、数字、`.`、`_`、`-`，release 必须是 `releases_path` 的直接子目录，`current_path` 必须不存在或为符号链接。preflight 必须验证架构、release/current 父目录写权限、可用磁盘和最多 64 个依赖。`stop` 可选，`start` 和 `health` 必填，`rollback_start` 默认复用 `start`；命令不经过 shell 且每步最多 300 秒。服务停止后原子切换 current symlink，启动或健康检查失败时必须在同一 Agent 请求内切回旧链接并恢复旧服务，返回 `deployed`、`stop_failed`、`rolled_back` 或 `rollback_failed` 结构化状态。
- `apply_patch` 仅更新一个已存在的普通 UTF-8 文本文件，补丁路径必须与请求路径完全一致；补丁最大 256 KiB，目标文件最大 16 MiB，最多 128 个 hunk，不支持创建、删除、重命名或无上下文纯插入。旧侧上下文必须唯一匹配，可用 `expected_sha256` 检测冲突；整个补丁成功前不得修改目标，并保留 BOM、原有行尾和末尾换行状态。
- `read_file_lines` 使用 1-based inclusive 行号，`start_line` 默认 1，未提供 `end_line` 时默认读取 200 行；单次最多 10,000 行、返回 1 MiB，并将为定位起始行而扫描的内容限制为 64 MiB。只接受非符号链接的普通文件，请求范围必须是 UTF-8。
- `list_files` 对目录项排序并分页，`limit` 默认 200、最大 1,000；`recursive` 默认 false，递归时 `name` 为相对请求目录的 `/` 分隔路径，`max_depth` 默认 16、最大 64。可用最大 1 KiB 的 `pattern` glob 过滤相对路径；符号链接可列出但不遍历，单次最多扫描 100,000 个目录项、输出 1 MiB。
- `grep` 对单个普通文件或目录树中的普通 UTF-8 文件执行逐行 Rust 正则搜索，大小写敏感默认开启，可用最大 1 KiB 的 `glob` 过滤相对路径。正则最大 4 KiB，结果默认 200 条、最多 1,000 条，单文件默认扫描 1 MiB、最多 16 MiB，单次总计最多枚举 100,000 个目录项、递归 64 层、扫描 10,000 个文件或 64 MiB、输出 1 MiB；匹配文本最多保留 1 KiB。目录搜索不跟随符号链接，并跳过 `.git`、`.hg`、`.svn`、`.next`、`node_modules`、`target`、`dist`、`build`。
- 文件传输必须校验长度和 SHA-256，成功前使用同目录临时文件，失败时不得留下目标半文件。
- 工具输出必须有界。命令 stdout/stderr 各限制为 256 KiB，命令超时最大 300 秒。
- `system_info` 保留主机、内核、运行时间、负载、内存、系统盘和温度字段，并返回 `os`、`cpu`、`identity`、`network`、`filesystems`、`time`、`init_system`、`toolchains`。Linux 必须有界解析 os-release、CPU/ABI/libc、用户组/umask/capabilities、网卡/IP/路由/DNS/监听端口、mount/文件系统/inode/只读状态、系统时间/时区/init 和 PATH 中的固定工具清单；不得执行外部诊断命令。网卡最多 128 个、地址最多 512 个、路由最多 256 条、监听端口最多 512 个、mount 最多 256 个、工具链最多 24 个，各集合必须报告 `available` 和 `truncated`。
- `process_start` 不经过 shell，stdin 固定为空；后台任务默认超时 1 小时、最大 24 小时，同时最多保留 16 个。任务属于 agent 进程而非单个连接，可在 proxy 断线重连后继续查询，但不跨 agent 进程重启持久化。达到上限时先回收最早结束的任务；若全部仍在运行则拒绝启动。
- 后台任务 stdout/stderr 各保留最近 256 KiB。`process_output` 使用绝对字节游标，每路默认返回 64 KiB、最多 256 KiB；游标落后于保留窗口时必须报告截断及最早可用游标。`process_wait` 默认等待 10 秒、最长 30 秒；`process_close` 只释放已结束任务，运行中的任务必须先 signal 并 wait。
- `agent_info` 必须返回 Agent 版本、协议版本、构建 target/profile/Git revision、运行实例 ID/PID/启动时间、平台、支持操作、能力、限制和自更新路径。构建信息由 `crates/agent/build.rs` 注入；自检信息必须保持有界且可机器读取。
- `remote_status` 不得主动连接，必须返回 `connection_state`（`cached` 或 `disconnected`）、`lifecycle_state`（`ready`、`rebooting` 或 `updating`）、最近成功时间、最近错误、最近探测和缓存的 Agent 信息。`remote_probe` 主动连接或健康检查，返回可达性、延迟、连接复用情况、Agent 信息或结构化错误；探测超时默认 5 秒，范围 100..=30,000 ms。
- `wait_remote` 支持 `online`、`offline`、`offline_then_online`；正常状态默认等待在线，重启或更新状态默认等待离线后再在线。`offline_then_online` 可通过实际观察到离线或 Agent 实例 ID 变化确认。总超时默认 120 秒、范围 1..=600,000 ms，轮询间隔默认 1 秒、范围 100..=10,000 ms，每次探测沿用 `remote_probe` 的默认值和范围。
- `reboot` 延迟默认 1 秒、范围 250..=10,000 ms。Agent 必须先校验平台和权限，再确认请求并延迟执行；proxy 收到确认或在请求发出后观察到预期断开时丢弃会话并标记 `rebooting`，结果必须区分是否收到确认。除这些显式生命周期流程外，连接错误发生在请求发出后时仍按状态不确定处理，禁止自动重放。
- `agent_update` 的 `local_path` 属于 proxy 所在 PC。proxy 必须先主动探测，将普通候选文件上传到 Agent 公布的固定 staging 路径并校验 SHA-256；候选必须通过 `--self-check`，且协议版本和构建 target 与当前 Agent 兼容。独立 helper 等待旧 Agent 退出后原子替换可执行文件并保留 rollback 文件；新 Agent 成功绑定监听并稳定运行后才能清理备份，启动失败必须恢复旧程序并重启。结果必须明确区分 `updated`、`rolled_back`、`timed_out` 和 `unconfirmed`；等待及探测参数范围与 `wait_remote` 相同。

## 平台行为

- Linux 提供全部工具；`reboot` 使用 `reboot(2)` 并要求 Agent 以 root 运行。
- Windows/macOS 提供通用文件工具、基础文件变更、可恢复文件传输、目录同步和 `exec`；Unix 还提供 `chmod`、Unix mode 和 `deploy_release`。Windows 还提供 `kill` 和 `system_info`，其 `chmod` 和带 mode 的创建/上传返回结构化 unsupported。
- `agent_info` 和 proxy 生命周期编排工具不受目标平台限制。Windows 的 `reboot` 使用 `shutdown.exe /r /t 0 /f`，macOS 的 `reboot` 返回结构化 unsupported。
- macOS/Unix 可提供 `sh_exec`、`kill` 和 `system_info`；Windows 的 `sh_exec` 固定使用 `C:\Program Files\Git\bin\bash.exe --noprofile --norc -c`，不搜索 PATH 或回退到其他 shell，Git Bash 不存在时返回结构化 unsupported。Windows 的 `kill` 仅接受 signal 9 或 15，两者均强制终止进程。
- Windows 的 `exec` 使用 Job Object 管理命令进程树，超时必须终止命令及其后代，不得遗留持有输出管道的子进程。
- Ingenic XBurst 等小端 MIPS32r2 Linux 设备使用 `targets/mipsel-unknown-linux-musl.json` 构建 Agent；目标固定为 o32、soft-float 和静态 musl，因 Rust 为 Tier 3 目标，必须使用带 `rust-src` 的 Nightly `-Z json-target-spec -Z build-std=std,panic_abort` 构建，且不得通过 `cargo zigbuild` 直接映射该 Rust 三元组。
- 后台任务在 Unix 使用独立进程组，在 Windows 使用 Job Object；`process_signal` 在 Unix 接受 1..=64，Windows 仅接受 9 或 15 且两者都终止整个 Job Object。Agent 的任务管理器释放时必须终止仍在运行的任务并回收工作线程。
- `pids`、`process_info` 支持 Linux、Windows 和 macOS；macOS 使用原生 libproc/sysctl 接口，无法读取的命令行返回空字符串。
- `pkill` 支持 Linux、Windows 和 macOS，按平台进程名完整匹配（Windows 不区分大小写）并排除 agent 自身 PID；Linux `/proc/<pid>/comm` 名称最多 15 字节，macOS `pbi_name` 名称最多 31 字节，Windows 快照名称最多 260 个 UTF-16 单元。默认 signal 15，Windows 仅接受 9 或 15；最多匹配 1024 个目标，超过上限时不得发送任何信号。
- Windows 进程枚举遇到不可读取的进程时保留 PID 和可用字段，命令行返回空字符串；`process_info` 的 `state`、`uid` 返回 `null`。
- Windows `system_info` 返回主机、系统版本、运行时间、内存、系统盘、CPU、网卡、当前用户名、系统时间和可用工具链；无 Unix load average 和统一温度接口，分别返回零值和空数组，Linux 专属集合标记为不可用。macOS `system_info` 返回主机、内核/系统版本、运行时间、CPU、用户组、网卡/IP、DNS、内存、根文件系统、时间和工具链，并通过 `getloadavg` 提供真实负载；无统一温度接口，温度返回空数组，尚未原生采集的路由和监听端口标记为不可用。
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
cargo +nightly build -p remote-ops-agent --release --target targets/mipsel-unknown-linux-musl.json -Z json-target-spec -Z build-std=std,panic_abort
```

涉及发布产物时，再执行对应目标的 release build 或 `cargo zigbuild`。
