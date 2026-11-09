# RemoteOps 项目约束

## 项目结构

- `crates/protocol`：proxy 与 agent 共享的认证、分帧和 wire 类型。
- `crates/proxy`：PC 端 MCP stdio server、本地文件 I/O 和远端连接客户端。
- `crates/proxy/src/mcp.rs` 仅处理 JSON-RPC；`mcp/catalog.rs` 是工具名称、注解和批调用策略的唯一注册表；`input.rs`/`output.rs` 定义 schema，`parameters.rs` 定义校验，`dispatch.rs` 只执行已预检调用，`batch.rs` 负责整批预检和有序执行。新增工具必须补齐穷尽匹配及合法参数契约测试，不得恢复未校验的默认转发分支。
- `crates/protocol/src/arguments.rs` 与 `execution.rs` 共享基础文件/进程/命令参数和校验；proxy 和 Agent 分别调用校验，Agent 继续负责文件类型、权限等远端状态检查。
- `crates/agent`：远端监听服务、文件传输落盘和平台工具实现。
- `crates/proxy/tests/end_to_end.rs`：真实双端连接、二进制往返和 stdio 测试。
- `deploy/remote-ops-agent.service`：Linux 下 systemd 部署的推荐单元，包含自更新必需的 `KillMode` 与 `Restart` 设置。

## 架构边界

- MCP JSON-RPC 只存在于 proxy 的 stdio 一侧，二进制文件内容不得编码进 MCP JSON。
- proxy 主动连接一个远端 agent；Agent 同一时刻只允许一个已认证 proxy 作为活动管理者，首版协议串行处理请求，不支持复用或自动重放。
- 新 proxy 完成认证后可原子接管空闲活动管理连接，Agent 必须关闭被取代的旧连接；认证失败不得影响当前管理者。活动管理者从确认请求到完整响应发送期间均为 busy，此时新候选的首个请求必须返回 `manager_busy` 并保证未执行，不得中断或排队旧请求。候选连接和 handler 数量必须有界。
- 认证后的活动连接不得因为空闲而关闭；内部所有权轮询不得改变该语义。握手、已开始帧、传输帧间隔和 socket 写入分别保持有界超时，并启用 TCP Keepalive；proxy 可在用户请求发送前通过内部 Request/Response 健康检查安全重连。
- remote_probe 及 wait_remote 的单次探测使用同一个绝对截止时间，覆盖 TCP connect、完整握手、健康请求写入、等待首字节及完整响应帧，不能让每个阶段重新获得一份预算。复用连接时第一轮截止时间至多为总预算的一半，为新连接留出剩余时间；连接重建后 connection_reused=false。普通握手也必须在总 handshake_timeout 内结束，不能被逐字节输入延长。带截止时间的请求不得修改认证连接的正常空闲语义。
- 控制消息使用 JSON，文件内容使用 `Chunk` 帧。修改 wire 类型或帧格式时必须提升协议版本并补兼容/拒绝测试。
- proxy 和 agent 固定使用 `BUILTIN_PSK = b"JARK006_PSK"`，不得要求部署者配置 `REMOTE_OPS_PSK`。帧完整性与防重放保护仍须保留；协议不承诺认证安全或机密性。
- stdout 是 proxy 的 MCP 协议通道，任何日志只能写 stderr。
- proxy 的 MCP stdio 单行请求最多 40 MiB、编码后响应最多 128 MiB；超过请求上限必须有界丢弃当前整行并继续读取下一条，非法 UTF-8 返回 parse error，不得让进程退出。响应编码必须有界；超限后返回明确的 response_too_large 工具错误并保持 stdio 可用，不能自动重放已执行的操作。
- 2 MiB 控制帧上限按 JSON 编码后字节数计算，含控制字符转义与有损 UTF-8。已开始帧的超时/EOF 必须与纯空闲轮询超时区分；半帧失败必须关闭连接，不能继续从下一字节解释帧。序列化和 Chunk 发送也必须在分配/写入前执行上限检查。

## 工具契约

proxy 暴露 40 个工具。修改名称、参数、默认值、上限或结果字段时，必须同步更新 schema、测试、README 和本文件。

- 每个工具都必须先在 proxy 完成类型、未知字段、格式、范围和参数关系预检，再建立连接、探测或修改状态。预检不得执行文件或命令操作；只有已预检的调用才能进入执行模块。依赖远端环境或文件状态的检查仍由 Agent 独立执行。
- proxy 错误详情统一为 `{kind,message,source,outcome,request_started,prior_requests,recovery?}`，source 为 proxy/remote/transport/protocol，outcome 为 not_sent/unknown/remote_error/rejected/response_limited/local_error。prior_requests 为该工具编排中最后失败请求之前的请求数，可能已产生效果；可选 recovery 返回失败阶段、路径、清理确认和停止位置，不能丢失已有恢复上下文。只有 proxy 发送前 invalid_params 返回 JSON-RPC -32602（error.data 含详情）；远端校验错误属于执行结果，返回 isError:true，content 的 JSON 为 `{error:详情}`，避免错误结果违反成功 outputSchema。batch 子错误、remote_status.last_error、remote_probe.error 使用同一详情。
- 完整、已认证、请求 ID 匹配且信封一致的普通远端错误可以保留串行连接，不更新最近成功时间；manager_busy 仍丢弃候选连接并明确 rejected。传输中的错误保守丢弃连接。请求发送后传输失败保持 unknown 并禁止重放；reboot/agent_update 只可将观察到的真实断开当作未收到确认的生命周期流程，超时、协议错误或远端同名错误不得冒充断开。

- 所有 MCP 工具接受 proxy 专用参数 `response_mode`（`compact`/`full`），默认 `compact`；proxy 启动参数 `--response-mode` 可改变默认值。选项必须在连接、修改状态或转发前校验并移除，不进入 wire 请求。`compact` 使用紧凑 JSON；`system_info` 省略 `network`、`filesystems`，`agent_info` 省略 `build`、`supported_operations`、`limits`、`update`，`remote_probe`/`remote_status`/`set_remote` 中的缓存 Agent 信息（含 `last_probe.agent_info`）使用同样规则。实际省略路径必须通过 `_response: {mode:"compact", omitted_fields:[JSON Pointer...]}` 显式报告；schema 必须同时接受 compact/full。其他工具不省略数据字段。文件正文、命令输出、错误、退出码、超时、实例 ID、分页、截断和恢复字段不得被精简。`full` 保留全部原始字段与格式化 JSON。

- 兼容工具：`read_text`、`read_file_lines`、`tail_text`、`write_text`、`apply_patch`、`list_files`、`grep`、`stat`、`file_hash`、`pids`、`process_info`、`kill`、`sh_exec`、`exec`、`system_info`。
- 进程工具：`pkill`。
- 环境发现工具：`environment`，被动返回平台、cwd、实际 shell 路径及 native 选择、PID 1、最多 24 个实用程序/服务管理程序和执行能力。最多检查 128 个 PATH 目录，跳过相对、非 UTF-8 和超过 4 KiB 的路径并报告不完整搜索；不执行外部诊断命令，不根据 CPU 架构或 BusyBox 存在推断“嵌入式”。`shells.detection=file_metadata` 只代表检测到文件，不保证 shell、BusyBox applet 或服务管理器可用。Linux init 身份依据 `/proc/1/comm` 和实际 `/proc/1/exe`；程序存在和 `/sbin/init` 链接不能代替正在运行的 init 证据。
- `sh_exec` 接受 `command`、`shell?`、`cwd?`、`env?`、`timeout_ms?`。shell 为 `native`（默认）、`sh`、`bash`、`busybox`、`pwsh`、`powershell`、`cmd`、`git_bash`；native 在 Unix 选 sh，在 Windows 优先 PowerShell 7 再选 Windows PowerShell 5.1，没有时返回 unsupported，不跨命令语言回退。显式选择不可用 shell 时返回 unsupported；结果额外返回 `shell:{name,program}`。command 最多 256 KiB UTF-8 且不得含 NUL，cmd 另限 8,000 个 UTF-16 单元；cwd 限 1..=4096 UTF-8 字节；env 最多 128 项、键和值总计 64 KiB。所有参数在 proxy 和 Agent 双重校验。超过 8 KiB 的 POSIX 脚本及所有 PowerShell 脚本通过临时文件执行并自动清理；短 POSIX 命令不依赖可写临时目录。PowerShell 脚本使用 UTF-8 BOM，固定关闭 profile/交互并设置 UTF-8 输出；Bash/Git Bash 关闭 profile、rc、BASH_ENV、ENV，sh/BusyBox 清除 ENV；cmd 使用 `/D /S /C`。原生程序和 cmd 输出仍按有损 UTF-8 解码，不推断代码页。
- 后台任务工具：`process_start`、`process_output`、`process_wait`、`process_signal`、`process_close`。
- 传输工具：`upload_file`、`download_file`。
- 基础文件变更工具：`mkdir`、`remove`、`move`、`copy`、`chmod`、`symlink`。
- 目录同步与发布工具：`sync_directory`、`deploy_release`。
- Agent 生命周期工具：`agent_info`、`reboot`。
- proxy 本地管理与生命周期编排工具：`remote_status`、`set_remote`、`remote_probe`、`wait_remote`、`agent_update`。`remote_status` 被动查询地址、缓存连接和生命周期状态；`set_remote` 可单独设置 IPv4 或端口，配置仅在当前进程内生效，并且必须总是丢弃缓存会话（含地址未变化时），作为确定性的重连手段，但地址未变化时不得清除观测和生命周期状态；其余工具负责主动探测、等待状态变化和编排 Agent 自更新。
- 批量执行工具：`batch` 在一个 MCP 调用内按顺序执行至多 16 个检查与命令工具（`read_text`、`read_file_lines`、`tail_text`、`list_files`、`grep`、`stat`、`file_hash`、`pids`、`process_info`、`system_info`、`environment`、`agent_info`、`remote_status`、`sh_exec`、`exec`），子结果按输入顺序返回 `{tool, ok, result|error}`。清单和注解从 catalog 生成。所有子参数和响应选项必须在执行首项前完成 proxy 预检，任一预检失败整体返回 `invalid_params`，不得连接或执行任何子项。普通远端运行时失败仍逐项报告；transport/protocol 错误、unknown、manager_busy、response_limited 必须停止后续子项并返回 not_executed（outcome=not_sent，recovery.stopped_after 为 1-based 停止位置），不得重连后继续命令。`sh_exec`/`exec` 能修改状态，必须声明 destructiveHint，不能承诺只读、事务或回滚。子调用 arguments 默认 `{}`，其 response_mode 可覆盖外层模式。batch 仍串行发送多个远端请求，不新增协议操作。
- `upload_file`、`download_file`、`sync_directory` 和 `deploy_release` 的 `local_path` 均属于 proxy 所在 PC。
- 单文件默认上限 4 GiB，chunk 上限 64 KiB；控制帧上限 2 MiB。
- 远端协议版本为 4，增加环境发现和显式 shell/cwd/env 契约，v3 握手和帧必须拒绝。proxy 与 Agent 必须配套升级，v3 Agent 需通过现有连接上传后人工替换，不能用 v4 proxy 的 agent_update 跨协议连接。上传请求必须携带完整 SHA-256，可选 `mode` 范围为 `0..=0o7777` 且只在 Unix Agent 生效；上传和下载的 `resume` 默认 false。续传必须校验 partial 文件的已有长度和前缀 SHA-256，最终仍校验完整长度和 SHA-256；前缀不匹配的下载安全回退到偏移 0。成功前使用同目录 partial 文件，失败时仅在启用续传时保留可恢复 partial，目标不得出现半文件。
- `mkdir`、`remove`、`move`、`copy`、`chmod`、`symlink` 必须使用精确路径并区分普通文件、目录、符号链接和特殊文件。递归删除和复制最多检查 100,000 个条目；递归复制拒绝符号链接和特殊文件，递归删除不跟随符号链接并拒绝特殊文件。`move` 不得在跨文件系统时隐式 copy-delete；目录覆盖始终拒绝，非目录覆盖必须显式开启。
- `sync_directory` 基于排序 manifest、大小和 SHA-256，只传变化普通文件；本地符号链接、特殊文件和非 UTF-8 相对路径必须拒绝。同步在目标同级 staging 构建，提交前逐项复核类型、大小和哈希，再将旧目标改名为 `backup_path` 并切换 staging；旧 backup 不得静默删除。远端 Agent 支持时保留文件、目录和同步根目录 Unix mode，不支持时 proxy 在 manifest 中省略 mode 并正常同步。manifest 默认最多 4,096 个条目、4 GiB、32 层，最大 10,000 个条目、4 GiB、64 层；排除 glob 最多 64 个、每个 1 KiB。
- 同步上传/提交状态不确定时禁止自动重连发送 sync_abort；保留 staging/manifest 供检查，错误 recovery 必须报告阶段和路径。仅确定性的提交前失败可尝试 abort，并报告清理是否确认。proxy 必须校验完整 required_uploads 计划和绑定目标名称的随机 sibling 路径后才上传。Agent 提交前重新检查目标类型；fsync 文件/目录后才激活，mode 延后应用，避免 mode=000 让 staging 在校验前不可读。成功结果增加 commit（Agent 原始确认），committed=true 与 durable=false 必须明确区分；切换失败时保留 backup 并报告恢复失败，不忽略 rename 错误。清理先恢复根目录和父目录权限，再检查子目录类型，不能通过替换的 symlink chmod 外部路径。
- `deploy_release` 仅在 Unix Agent 上支持。`release_id` 限 1..=128 个 ASCII 字母、数字、`.`、`_`、`-`，release 必须是 `releases_path` 的直接子目录，`current_path` 必须不存在或为符号链接。preflight 必须验证架构、release/current 父目录写权限、可用磁盘和最多 64 个依赖。`stop` 可选，`start` 和 `health` 必填，`rollback_start` 默认复用 `start`；命令不经过 shell 且每步最多 300 秒。服务停止后原子切换 current symlink，启动或健康检查失败时必须在同一 Agent 请求内切回旧链接并恢复旧服务，返回 `deployed`、`stop_failed`、`rolled_back` 或 `rollback_failed` 结构化状态。
- 部署命令的 spawn/cwd 等执行错误也必须进入结构化步骤和回滚，不能通过 ? 提前绕过回滚。rollback_switch 或 rollback_start 失败必须返回 rollback_failed；rolled_back 表示旧链接是否已恢复。current 在事务中被换为普通文件时不得覆盖/删除。新链接指向解析后的 release 绝对路径，避免相对路径因 current 父目录不同而失效。
- `apply_patch` 仅更新一个已存在的普通 UTF-8 文本文件，补丁路径必须与请求路径完全一致；补丁最大 256 KiB，目标文件最大 16 MiB，最多 128 个 hunk，不支持创建、删除、重命名或无上下文纯插入。旧侧上下文必须唯一匹配，可用 `expected_sha256` 检测冲突；整个补丁成功前不得修改目标，并保留 BOM、原有行尾和末尾换行状态。
- `read_file_lines` 使用 1-based inclusive 行号，`start_line` 默认 1，未提供 `end_line` 时默认读取 200 行；单次最多 10,000 行、返回 1 MiB，并将为定位起始行而扫描的内容限制为 64 MiB。只接受非符号链接的普通文件，请求范围必须是 UTF-8。
- `list_files` 对目录项排序并分页，`limit` 默认 200、最大 1,000；`recursive` 默认 false，递归时 `name` 为相对请求目录的 `/` 分隔路径，`max_depth` 默认 16、最大 64。可用最大 1 KiB 的 `pattern` glob 过滤相对路径；符号链接可列出但不遍历，单次最多扫描 100,000 个目录项、输出 1 MiB。每个条目返回 `name`、`kind`、`size`、`mtime`（Unix epoch 秒）、`mtime_iso`（RFC 3339、Agent 本地时区）和 `mode_str`（`ls -l` 风格权限串）；原始值为精确计算用，`_iso`/`_str` 为派生展示字段。
- `stat` 不跟随符号链接读取元数据，返回 `size`（字节）、`mtime`（Unix epoch 秒）、`mtime_iso`（RFC 3339、Agent 本地时区）、`mode`（原始 st_mode，平台无 Unix mode 时为 0）、`mode_str`（`ls -l` 风格的 10 字符类型与权限串）和 `kind`（`file`/`dir`/`symlink`/`other`）。`mtime`、`mode` 为原始值用于精确计算，`mtime_iso`、`mode_str` 为派生展示字段，任何修改都必须同步更新 proxy schema、测试和 README。
- `grep` 对单个普通文件或目录树中的普通 UTF-8 文件执行逐行 Rust 正则搜索，大小写敏感默认开启，可用最大 1 KiB 的 `glob` 过滤相对路径。正则最大 4 KiB，结果默认 200 条、最多 1,000 条，单文件默认扫描 1 MiB、最多 16 MiB，单次总计最多枚举 100,000 个目录项、递归 64 层、扫描 10,000 个文件或 64 MiB、输出 1 MiB；匹配文本最多保留 1 KiB。目录搜索不跟随符号链接，并跳过 `.git`、`.hg`、`.svn`、`.next`、`node_modules`、`target`、`dist`、`build`。
- 文件传输必须校验长度和 SHA-256，成功前使用同目录临时文件，失败时不得留下目标半文件。
- 工具输出必须有界。命令 stdout/stderr 各限制为 256 KiB，命令超时最大 300 秒；`sh_exec` 和 `exec` 结果包含 `duration_ms`（毫秒）。`read_text`、`tail_text` 不得向返回文本追加任何截断标记，截断状态只通过 metadata 的 `truncated` 字段报告。
- 当 JSON 转义会超出帧预算时，read_text 返回更短前缀并推进 next_offset，read_file_lines 在完整行边界停止并返回 next_line，tail_text 保留可编码的末尾且保留 CRLF/LF/末尾换行，前台命令缩短保留前缀并设置对应 truncated。后台 process_output 可返回更短片段，next_*_cursor 必须只推进实际返回原始字节，不能丢弃尚未读取的保留输出。无法分页的超大结果返回 response_too_large；该状态表示操作可能已完成，不能当作未执行。
- read_text/tail_text/file_hash 与 read_file_lines 一样拒绝目录、符号链接和特殊文件；Unix 打开使用 O_NOFOLLOW/O_NONBLOCK 并复核普通文件，避免 FIFO 阻塞。file_hash 还必须防止扫描期间文件增长突破 max_bytes。下载在任何失败路径上，未启用 resume 时清理新建普通 partial；拒绝的既有 symlink/special partial 必须保持原样。
- `system_info` 保留主机、内核、运行时间、负载、内存、系统盘和温度字段，并返回 `os`、`cpu`、`identity`、`network`、`filesystems`、`time`、`init_system`、`toolchains`。Linux 必须有界解析 os-release、CPU/ABI/libc、用户组/umask/capabilities、网卡/IP/路由/DNS/监听端口、mount/文件系统/inode/只读状态、系统时间/时区/init 和 PATH 中的固定工具清单；不得执行外部诊断命令。网卡最多 128 个、地址最多 512 个、路由最多 256 条、监听端口最多 512 个、mount 最多 256 个、工具链最多 24 个，各集合必须报告 `available` 和 `truncated`。
- Linux `filesystems.mounts[].read_only` 必须同时检查 mountinfo 的挂载点选项和 superblock 选项，任一包含独立的 `ro` 即为只读；嵌入式 squashfs 可出现挂载点 `rw`、superblock `ro`，不得报告可写。
- `process_start` 不经过 shell，stdin 固定为空；后台任务默认超时 1 小时、最大 24 小时，同时最多保留 16 个。任务属于 agent 进程而非单个连接，可在 proxy 断线重连后继续查询，但不跨 agent 进程重启持久化。达到上限时先回收最早结束的任务；若全部仍在运行则拒绝启动。
- 后台任务 stdout/stderr 各保留最近 256 KiB。`process_output` 使用绝对字节游标，每路默认返回 64 KiB、最多 256 KiB；游标落后于保留窗口时必须报告截断及最早可用游标。`process_wait` 默认等待 10 秒、最长 30 秒；`process_close` 只释放已结束任务，运行中的任务必须先 signal 并 wait。
- `agent_info` 必须返回 Agent 版本、协议版本、构建 target/profile/Git revision、运行实例 ID/PID/启动时间、平台、支持操作、能力、限制和自更新路径；启动时间和 uptime 必须自进程启动起算（服务启动路径显式初始化运行身份），不得延迟到首次查询。构建信息由 `crates/agent/build.rs` 注入；自检信息必须保持有界且可机器读取。
- `remote_status` 不得主动连接，必须返回 `connection_state`（`cached` 或 `disconnected`）、`lifecycle_state`（`ready`、`rebooting` 或 `updating`）、最近成功时间、最近错误、最近探测和缓存的 Agent 信息。`remote_probe` 主动连接或健康检查，返回可达性、延迟、连接复用情况、Agent 信息或结构化错误；探测超时默认 5 秒，范围 100..=30,000 ms。复用已缓存会话时健康检查最多使用一半探测预算，必须为失败后的重连留出时间，不得因预算耗尽而放弃重连并误报不可达。
- `wait_remote` 支持 `online`、`offline`、`offline_then_online`；正常状态默认等待在线，重启或更新状态默认等待离线后再在线。`offline_then_online` 可通过实际观察到离线或 Agent 实例 ID 变化确认。用于比较的上一个实例 ID 在 `rebooting`/`updating` 编排状态下取自生命周期状态，否则取自最近缓存的 `agent_info`，保证 `ready` 状态下也能识别被替换的 Agent，不得因为没有编排状态而无法确认实例变化。总超时默认 120 秒、范围 1..=600,000 ms，轮询间隔默认 1 秒、范围 100..=10,000 ms，每次探测沿用 `remote_probe` 的默认值和范围。
- `reboot` 延迟默认 1 秒、范围 250..=10,000 ms。Agent 必须先校验平台和权限，再确认请求并延迟执行；proxy 收到确认或在请求发出后观察到预期断开时丢弃会话并标记 `rebooting`，结果必须区分是否收到确认。除这些显式生命周期流程外，连接错误发生在请求发出后时仍按状态不确定处理，禁止自动重放。
- `agent_update` 的 `local_path` 属于 proxy 所在 PC。proxy 必须先主动探测，将普通候选文件上传到 Agent 公布的固定 staging 路径并校验 SHA-256；候选必须通过 `--self-check`，且协议版本和构建 target 与当前 Agent 兼容。独立 helper 等待旧 Agent 退出后原子替换可执行文件并保留 rollback 文件；新 Agent 成功绑定监听并稳定运行后才能清理备份，启动失败必须恢复旧程序并重启。Agent 必须把监督自身的 systemd 单元名写入更新 manifest（无监督者时为空）；helper 验证新二进制健康后必须把 Agent 交回该单元（停止自己直接启动的临时实例并 `systemctl restart <unit>`、确认单元 active），回滚重启同样必须经该单元，保证更新后的 Agent 仍受监督且不与单元竞争监听端口。结果必须明确区分 `updated`、`rolled_back`、`timed_out` 和 `unconfirmed`；等待及探测参数范围与 `wait_remote` 相同。
- 自更新必须要求 Agent 的 update_receipts 能力。helper 使用 attempt_id/candidate_sha256/previous_instance_id 关联本次尝试，在 target.update-result.json 原子记录 pending 和最终 updated/rolled_back/rollback_failed/unconfirmed（最多 16 KiB）；agent_info.update.last_result 返回该回执，agent_update 结果增加 update_receipt。proxy 必须等待回执完成，临时候选上线不能当作更新完成；缺失/不匹配回执不确认成功。helper 在旧进程退出后复核候选哈希；回滚失败保留 backup/manifest 并提供路径。systemd 单元属性不符合 KillMode=process/Restart=on-failure 时，在退出旧 Agent 前拒绝更新；systemctl 操作必须限时，监督者丢失时不能静默退化为无监督运行。
- Linux helper 等待旧 Agent 退出时，必须有界读取 `/proc/<pid>/stat` 并识别 zombie/dead 状态；交互式 BusyBox shell 未 wait 的僵尸 PID 仍通过 kill(pid,0)，不能据此阻止更新。procfs 不可读取时保守退回存活探测。

## 平台行为

- Linux 提供全部工具；`reboot` 使用 `reboot(2)` 并要求 Agent 以 root 运行。以 systemd 托管时必须采用 `deploy/remote-ops-agent.service` 的 `KillMode=process` 与 `Restart=on-failure`：前者保证自更新时 helper 不被 unit 连带杀死，后者保证退出码 0 的自更新退出不会与 helper 竞争重启；缺失这两项时必须在旧 Agent 退出前拒绝更新。
- Windows/macOS 提供通用文件工具、基础文件变更、可恢复文件传输、目录同步和 `exec`；Unix 还提供 `chmod`、Unix mode 和 `deploy_release`。Windows 还提供 `kill` 和 `system_info`，其 `chmod` 和带 mode 的创建/上传返回结构化 unsupported。
- `agent_info` 和 proxy 生命周期编排工具不受目标平台限制。Windows 的 `reboot` 使用 `shutdown.exe /r /t 0 /f`，macOS 的 `reboot` 返回结构化 unsupported。
- macOS/Unix 可提供 `sh_exec`、`kill` 和 `system_info`；Windows shell 由 `environment` 发现并按显式 `shell` 选择。`pwsh` 可在有该程序的 Unix 上使用；`powershell`、`cmd`、`git_bash` 仅支持 Windows，`sh`、`bash`、`busybox` 仅支持 Unix，不把 Windows 的 bash.exe 当作 Unix shell 或隐式启动 WSL。`agent_info.supported_operations` 按平台过滤，Windows 不报告 chmod/部署操作，macOS 不报告 reboot；参数、权限和运行环境限制仍以实际执行结果为准。Windows 的 `kill` 仅接受 signal 9 或 15，两者均强制终止进程。
- Windows 的 `exec` 使用 Job Object 管理命令进程树，超时必须终止命令及其后代，不得遗留持有输出管道的子进程。
- Ingenic XBurst 等小端 MIPS32r2 Linux 设备使用 `targets/mipsel-unknown-linux-musl.json` 构建 Agent；目标固定为 o32、soft-float 和静态 musl，因 Rust 为 Tier 3 目标，必须使用带 `rust-src` 的 Nightly `-Z json-target-spec -Z build-std=std,panic_abort` 构建，且不得通过 `cargo zigbuild` 直接映射该 Rust 三元组。
- 后台任务在 Unix 使用独立进程组，在 Windows 使用 Job Object；`process_signal` 在 Unix 接受 1..=64，Windows 仅接受 9 或 15 且两者都终止整个 Job Object。Agent 的任务管理器释放时必须终止仍在运行的任务并回收工作线程。
- `pids`、`process_info` 支持 Linux、Windows 和 macOS；macOS 使用原生 libproc/sysctl 接口，无法读取的命令行返回空字符串。`process_info` 的 `start_time_seconds` 为开机后秒数，`start_time_iso` 为进程启动墙钟时间（RFC 3339、Agent 本地时区，Linux 无法确定时为 `null`）。
- pids/process_info 的 cmdline 最多保留 128 KiB UTF-8，增加 cmdline_available/cmdline_truncated；不可读取和实际空命令行必须可区分。pids 的 scan 返回 processes_scanned、cmdline_bytes_read、filter_incomplete；命令行不可读或被截断且进程名未匹配时，过滤结论必须标记可能不完整。每页最多检查 4096 个进程、读取 16 MiB 命令行、输出 1 MiB 编码 JSON；可以返回空 processes 但有 next_cursor，游标必须前进且不能跳过因输出预算尚未返回的匹配 PID。原有 limit 默认/最大 1024 不变。平台快照最多 65536 个 PID，超过或检测到快照缓冲截断返回 resource_limit，pkill 也不得基于不完整快照发信号。Linux status/stat 分别最多 64/16 KiB，启动时间来源 /proc/stat 最多 1 MiB；Windows VirtualQueryEx 最多 65536 个区域，超过时 virtual_bytes=null；macOS 原生命令行缓冲仍最多 4 MiB。
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

- 发布版本取自 workspace；当前配套发布版本为 2.0.1，远端协议保持 v4，支持从 2.0.0 通过 agent_update 升级；跨 v3 升级流程见 `docs/MIGRATING_V2.md`。
- `targets/releases.json` 是发布目标的唯一矩阵；`buildAll.ps1` 调用 `tools/build_release.py`。发布必须只收集 Cargo 报告的 executable，校验架构/ABI/内嵌构建来源与 SHA-256；任何失败或构建期间源码变化均不得交付完成目录。不得从固定 target 路径回退收集旧文件，不得覆盖已有发布目录。
- 构建 staging 使用与输出同级的 `.remote-ops-release-*` 目录，必须由 `.gitignore` 忽略，避免 `-Output Releases` 的临时文件使后续 target 被标记为 dirty；Git dirty 检测显式检查未跟踪文件，不受用户 status.showUntrackedFiles 配置影响。
- 构建源码来源标记由 `crates/build_version.rs` 注入，两个二进制的 `--version` 第二行保留该标记，便于不执行跨平台程序即验证来源。开发构建允许 `unknown`/`development`，发布构建必须匹配本次来源哈希和唯一 build ID。

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
python3 -m unittest discover -s tools -p 'test_*.py'
cargo check --workspace --target x86_64-unknown-linux-musl
cargo check -p remote-ops-agent --target aarch64-unknown-linux-musl
cargo check -p remote-ops-agent --target armv7-unknown-linux-musleabihf
cargo +nightly build -p remote-ops-agent --release --target targets/mipsel-unknown-linux-musl.json -Z json-target-spec -Z build-std=std,panic_abort
```

涉及发布产物时，再执行对应目标的 release build 或 `cargo zigbuild`。
