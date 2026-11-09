# 面向 AI Agent 的重构路线

目标是让 Agent 用更少的试探和上下文完成真实远端任务，并根据目标环境选择可靠操作。保留二进制传输、有界资源、原子文件提交、单管理连接和请求不自动重放等已有边界。此路线覆盖整体重构，单个阶段完成不代表整体目标完成。

## 验收场景

1. 首次连接后，用简短、机器可读的环境信息决定下一步；需要网络、磁盘等细节时显式展开。
2. 普通 Linux、BusyBox/无 systemd 的嵌入式 Linux、macOS 和 Windows 按实际能力工作。不能把程序存在、init 类型或平台名称当作能力已验证的证据。
3. Windows 明确选择 PowerShell 7、Windows PowerShell 5.1、cmd 或 Git Bash；Unix 明确选择 sh/bash 等可用 shell。返回实际执行环境，避免隐式切换命令语言。
4. 日常读取、搜索、执行、编辑和传输默认反馈紧凑，保留正文、错误、退出码、分页游标、截断状态和恢复信息；可按调用或 proxy 配置获取完整结果。
5. 长任务、连接接管、重启、更新与发布有明确状态；连接断开后不重放可能已执行的操作。
6. 工具发现和文档支持 Agent 正确选择工具；清理重复职责、误导描述和实现与 schema 的漂移。
7. 在真实双端协议、MCP stdio 和各目标构建上验证；无法运行的目标明确记录验证范围。

## 实施顺序与证据

| 阶段 | 交付 | 状态 |
| --- | --- | --- |
| 1. 响应层 | 独立呈现模块；compact/full；批调用继承与覆盖；schema/文档/真实 stdio 验证 | 已完成，见下方验证记录 |
| 2. 环境与执行 | 轻量能力画像；显式 shell；Windows 与嵌入式 Linux 差异；必要时升级 wire 协议 | 已完成，wire v4；见下方验证记录 |
| 3. 工具组织与边界 | 拆分巨型 MCP 模块；统一注册、校验和契约；审计批调用及冗余功能 | 已完成，40 个工具完整预检；见下方记录 |
| 4. 工作流可靠性 | 面向上述场景复核文件、任务、连接、同步、发布和生命周期实现 | 已完成，见 4a/4b/4c 和最终验收记录 |
| 5. 交付验证 | 跨平台构建、平台实测、使用示例及发布产物 | 已完成 11 目标构建和可用平台实测；硬件范围限制见最终验收 |

## 当前审计发现

- 非文本工具将 pretty JSON 同时放入 `content` 和 `structuredContent`，大型系统快照和缓存的 Agent 信息重复消耗上下文。
- MCP 层原为 3,834 行：阶段 3 已将 JSON-RPC 入口缩至 161 行，拆分注册、schema、预检、执行和 batch 模块。
- 原先 Windows `sh_exec` 固定使用 Git Bash：阶段 2 已提供明确 shell 选择及被动发现。
- 原先 `agent_info.supported_operations` 使用全平台静态列表：阶段 2 已按平台过滤，并增加 unix_mode/shell_selection/environment_profile；具体程序、权限和 applet 仍以实际执行结果为准。
- `batch` 的“一个往返、只读”误导描述与注解已在阶段 1 修正；阶段 3 又修复了前面的命令可能在后续参数报错前执行的问题。
- 起始 README 为 38、实现为 39 个工具：阶段 1 已同步，阶段 2 增至 40；阶段 3 数量保持 40。
- 双端测试确认的“普通远端错误也丢弃连接”已在阶段 4a 修复；错误来源、发送状态与结果确定性现在独立记录。

## 设计决定

- 响应选项由 proxy 消费，不进入远端请求；阶段 1 保持 wire v3，已有 Agent 可继续使用。
- compact 只省略明确列出的昂贵诊断分支，不截断命令输出或文件正文；省略字段必须在结果中显式列出，不能与数据截断混淆。
- full 保留原始结果，便于诊断和既有自动化；MCP 仍提供文本回退和结构化数据，兼顾不同客户端。
- 环境分类以检测到的能力为依据，不根据架构或发行版猜测“嵌入式”。

## 阶段 1 验证记录（2026-11-09）

- Windows：`cargo test --workspace`，118 项通过；WSL Ubuntu：同一工作树 `cargo test --locked --workspace --target-dir /tmp/remote-ops-response-validation`，114 项通过，包含 Unix 发布回滚与真实双端测试。
- `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings` 通过。
- x86_64 Linux musl workspace、aarch64 Linux musl Agent、armv7 Linux musl Agent 的约定 `cargo check` 均通过。
- Nightly + rust-src 的 MIPS32r2 release build 通过；产物 ELF 头、MIPS ABI flags 和 program headers 确认为 ELF32、小端、MIPS32r2、o32、soft-float、无动态解释器。链接器提示 Tier 3 目标没有预装 rustlib 目录；本构建使用 build-std。尚未在 MIPS 真机执行。
- 新增真实 MCP stdio + wire v3 契约测试：39 个输入/输出 schema 编译；compact/full 实际响应校验；批调用继承及覆盖；错误保留；文件换行保留；缓存不被呈现策略修改；省略标记约束。
- Windows 当前机器上，`system_info` 完整 MCP 响应约 10,579 字节，compact 约 4,728 字节，减少约 55%。这是字节数对比，不是 token 数或所有设备的保证。
- 为 schema 契约测试增加仅开发使用的 `jsonschema`；移除 proxy 没有 build script 却声明的 `chrono` build dependency。
- 阶段 1 结束时，环境画像、shell 选择和后续审计尚未完成；阶段 2 的后续进展如下，整体重构仍在进行。

## 阶段 2 验证记录（2026-11-09）

- 增加 `environment`，共 40 个工具；被动报告实际 shell 路径、native 选择、PID 1、最多 24 个实用程序和执行能力。PATH 搜索最多 128 个绝对目录，明确报告不完整搜索，不执行探测命令、不猜测嵌入式/服务器类别。
- `sh_exec` 增加 shell/cwd/env，返回实际解释器。Unix native=sh；Windows native=pwsh→Windows PowerShell，无候选时拒绝，不转到另一语言。共享 ShellArgs/ShellKind 和限额由 proxy/Agent 双重校验，wire 升为 4，并覆盖 v3 握手/帧拒绝。
- 通过真实 stdio/wire 验证 Windows PowerShell 7、5.1、cmd、Git Bash，以及 Linux/macOS sh/bash。测试包含中文与引号、cwd/env、明确退出码、PowerShell 错误文本、启动钩子禁用和超过 Windows/Unix 单参数限制的长脚本。期间修复 Windows 临时文件共享冲突、PowerShell CLIXML 错误输出、cmd 混合路径分隔符错误。
- Windows workspace 126 项、WSL Linux workspace 122 项、用户提供的 Apple M1 Mac workspace 121 项测试通过。三者 Clippy 均通过。另有一项 opt-in live Unix 测试通过 PC→Mac 的实际网络执行、补丁、读取、batch、180 KB 二进制上传/下载和 SHA-256 校验。
- Mac 使用原版 v3 remote-ops 上传源快照、原生编译测试，在独立临时端口启动 v4 release Agent。实测后已终止临时进程并关闭后台任务；原版服务仍为原实例（PID 16096、protocol 3）。原生 arm64 Agent/proxy release 构建成功，下载到本地 `target/validation` 并验证 Mach-O arm64；测试源码与构建缓存暂存于 Mac `/tmp/remote-ops-v4.KFvEKInB` 供后续回归使用，尚未替换常驻服务。
- `tools/verify_embedded_environment.py` 在 WSL 中建立临时 chroot，只放静态 musl Agent、BusyBox 和 sh 链接；不挂载 procfs、不提供 systemd/systemctl 或普通发行版工具。验证发现结果、默认与显式 BusyBox shell、cwd/env、非零退出码和 140 KB 长脚本。真实 /dev/null 以临时 bind mount 提供，进程和挂载均自动清理。这验证了精简用户空间，不代表 MIPS 真机验证。
- x86_64/aarch64/armv7 Linux musl 检查通过；x86_64 musl Agent release 实际运行于上述 chroot；MIPS Nightly build-std release 通过，产物再次验证 ELF32 小端 MIPS32r2/o32/soft-float、无动态解释器。MIPS 仍未在真机执行。
- v3→v4 需要配套替换 proxy/Agent。README/AGENTS 已明确迁移方法；本阶段不把跨协议自更新或 Mac 常驻部署算作已完成。
- 下一阶段仍需拆分 MCP 注册/schema/校验与执行代码、审计工具职责和重复项，再完成可靠性与最终交付验证；整体目标保持 active。

## 阶段 3 验证记录（2026-11-09）

- MCP 入口从 3,834 行缩至 161 行。`mcp/catalog.rs` 统一工具身份、名称、注解和 batch 策略；input/output/dispatch 对同一工具枚举做穷尽匹配，移除“未匹配工具默认转发”以及重复的名称/批调用清单。工具仍为 40 个。
- 参数预检和执行分离：只有已预检调用能进入执行模块。补齐原先直接转发的 read_text/tail_text/write_text/stat/file_hash/pids/process_info/kill/exec；system_info 等空参数工具也在 proxy 拒绝未知字段。9 个基础参数 DTO 与默认值/校验移至 protocol；exec 和后台进程共用程序、argv、cwd、env 校验，Agent 仍独立检查远端状态。
- batch 先准备全部子调用，再执行首项；任一 proxy 参数预检失败整批拒绝，不连接、不执行命令。远端状态导致的运行时错误仍按输入顺序逐项返回，不承诺事务或回滚。默认参数、response_mode 继承/覆盖保持原契约。
- 保存改造前后的实际 tools/list 快照于 `target/validation/catalog-before-stage3.json`、`catalog-after-stage3.json` 并逐项比较。40 个工具的输入/输出字段、默认值、限额和注解保持一致；只有 batch 描述和允许工具枚举顺序改变。wire 保持 v4，没有新增协议字段或依赖。
- 新增覆盖全部 40 个工具的合法参数/schema/预检测试、全部工具未知字段拒绝测试、基础参数双端拒绝测试，以及“首项命令有效、后项参数无效”的 batch 零连接测试。完整 workspace：Windows 130 项、WSL Linux 126 项、Apple M1 Mac 125 项通过；三者 Clippy 通过，格式与 diff 检查通过。
- 在用户 Mac 上复用独立测试工作区上传 SHA-256 为 `bc8d7360b99de41cc0c72557bffba65e25af9f59254d9ce580a75e53e324fd25` 的源码快照，原生执行完整测试和 Clippy。另以新版 PC proxy 连接阶段 2 的 v4 release Agent，通过真实网络 shell/补丁/二进制往返，验证本轮没有破坏 v4 互通。后台测试任务与临时 Agent 已结束并关闭；常驻 v3 Agent 未替换。
- x86_64/aarch64/armv7 musl 检查、MIPS Nightly build-std release 均通过；MIPS ELF32 小端 MIPS32r2/o32/soft-float、无动态解释器再次验证。尚未增加 MIPS 真机证据。
- 整体目标仍 active。阶段 4 接下来区分远端确定性错误与连接状态不确定，复核帧/stdio 资源上限和文件、任务、生命周期工作流；阶段 5 的最终发布产物与交付审计尚未完成。

## 阶段 4a 验证记录（2026-11-09）

- 错误包含 kind/message/source/outcome/request_started/prior_requests。proxy 参数错误只有在发送前才是 JSON-RPC -32602；远端 invalid_params 作为 MCP 工具错误返回。普通、完整、匹配请求 ID 且信封一致的远端错误保留连接；manager_busy 和传输错误仍断开。请求发送后失败保留 unknown，禁止重放。缓存 last_error、探测错误和 batch 子错误统一使用同一详情，schema/README/AGENTS 同步。
- ClientError 内部来源与错误 kind 分离，远端返回同名 connection_uncertain 不能冒充网络断开。reboot/agent_update 只将实际 EOF/reset 等断开用于未确认流程；纯超时和协议错误不误报确认。连接重建后的 remote_probe.connection_reused 现在为 false。
- 普通请求在连接前按 JSON 编码后长度检查，防止控制字符转义后超过 2 MiB；batch 同样先检查全部子请求。协议序列化采用有界 writer，发送 Chunk 也检查 64 KiB 上限。半帧超时变为独立 IncompleteFrame 错误，管理者轮询不能将其当作空闲超时。
- stdio 改为有界逐行读取，40 MiB 请求上限、128 MiB 编码后响应上限；超大输入丢弃整行后继续、非法 UTF-8 返回 parse error。超大输出返回 response_too_large，不终止 stdio，不重放已执行命令。纯展示转换和最终 stdio 编码均有上限。
- read_text、read_file_lines、tail_text、前台命令和后台输出按 JSON 编码预算切片；原始 byte/line/cursor 元数据继续报告实际进度。测试以 1 MiB NUL 文件、双路各 256 KiB NUL 命令输出验证帧不超限、管道继续排空、后台输出分两次取全。tail_text 保留 CRLF/LF 与末尾换行。不可分页的超大普通结果返回明确的 response_too_large，同时保持同步连接。
- 文本/哈希读取统一拒绝目录、symlink、FIFO 和特殊文件，Unix 使用 O_NOFOLLOW/O_NONBLOCK 后复核类型；tail 和 hash 扫描有界，防止文件增长突破扫描上限。下载 partial cleanup 覆盖连接失败等早退，Windows 文件先关闭再清理；启用 resume 时保留普通 partial，拒绝的预存 symlink 保持原样。
- 最终 workspace 测试：Windows 143 项、WSL Linux 141 项、Apple M1 Mac 140 项通过；另有已授权 PC→Mac live 测试通过，覆盖 remote_error 后继续 batch/复用连接，以及原有 shell/补丁/二进制往返。三平台 Clippy、格式检查通过。Windows 日志为 `target/validation/tests-stage4a-windows.log`，Linux 日志为 `/tmp/remote-ops-stage4a-final.log`。
- Mac 最终源码快照 SHA-256 为 `bed3bab745522409c4ce2b39f6589435403e995bc4854599d868db36f03e50ab`，原生测试/Clippy 通过；实测使用临时 v4 release Agent，未替换常驻 v3 服务，临时 Agent 和后台测试任务均关闭。
- x86_64/aarch64/armv7 Linux musl 检查及 MIPS Nightly build-std release 通过。本轮只改进现有消息的有界生成和解释，没有改变 v4 wire 类型/帧格式；MIPS 仍缺真机执行证据。
- 阶段 4 尚未完成。下一批必须继续检查进程枚举/进程信息的采集与输出上限、remote_probe 从握手到半帧读取的总超时预算、同步/发布/自更新的失败恢复与清理。现有一般测试不能证明这些工作已完成；阶段 5 发布构建脚本与最终交付审计也仍待处理。

## 阶段 4b 验证记录（2026-11-09）

- remote_probe 的新建连接、握手、健康请求写入、等待首字节和完整响应帧共用绝对截止时间；缓存健康检查最多使用总预算前半段，失败重连沿用剩余截止时间。wait_remote 的每轮已截短预算现在能实际约束整次探测。带预算 I/O 不修改会话的正常空闲策略；普通 client/server 握手也使用整体 handshake_timeout，逐字节输入不能反复续期。
- 新增可运行 socket 测试：慢速 ClientHello、慢速服务端 nonce、已开始的响应帧、socket 写背压、延迟握手后无响应、缓存健康检查只收到一个字节后成功重连，以及截止时间操作后的正常空闲接收。未改认证内容、帧格式或序列号，wire 仍为 v4。
- pids 三平台共用有界分页，默认/最大 limit=1024 不变。每页最多检查 4096 个进程、读取 16 MiB 命令行缓冲、返回 1 MiB 编码 JSON；全平台 PID 快照最多 65536 项，超限拒绝。空过滤页能返回继续游标；输出预算耗尽时不跳过尚未返回的匹配 PID。限制常量由 protocol 共享给 Agent 与 proxy schema。
- pids/process_info 增加 cmdline_available/cmdline_truncated，保留最多 128 KiB UTF-8 前缀。pids.scan 包含 processes_scanned/cmdline_bytes_read/filter_incomplete，避免把不可读或被截断命令行上的无匹配误报为完整过滤结论。Linux status/stat 分别限 64/16 KiB、启动时间来源 /proc/stat 限 1 MiB；Windows 内存区域统计最多 65536 区域，达到上限则 virtual_bytes=null；macOS 继续有界使用原生 libproc/sysctl。pkill 在快照不完整或超限时不发送信号。
- 新增编码后分页不丢 PID、空过滤页推进游标、命令行可用/截断元数据测试；Linux 真实启动约 192 KiB argv 进程，验证 process_info/pids 的截断前缀后终止进程组。真实双端响应再次通过 proxy outputSchema 校验。README/AGENTS 和工具描述同步更新；增加的是操作结果元数据，RemoteRequest/RemoteResponse/文件传输 wire 结构保持 v4。
- 完整 workspace：Windows 153 项、Linux 152 项、Mac 150 项通过。三平台 Clippy 和 Windows 格式检查通过；最后增加“进程名不可读时过滤不完整”的保守处理后，三平台定向分页回归再次通过。Windows 完整日志位于 `target/validation/tests-stage4b-windows.log`，Linux 完整日志位于 `/tmp/remote-ops-stage4b-final.log`。
- Mac 源码快照 `source-stage4b.tar.gz` SHA-256 为 `8b95d5bf980e47d6f2dc5041fd82a8c2d55909fe153b326167ab6d268d17c9f7`，原生测试/Clippy/release 构建通过；后续更新的 snapshot.rs SHA-256 为 `8177fcdf17c850155da402086776c48b57e9825ff581c5fa6ff7e4d640c5050f`。PC→Mac 独立测试端口实测新进程字段、分页、主动健康检查和已有 shell/补丁/传输流程均通过；测试 Agent 已停止，原版 v3 Agent PID 16096/实例 ID 保持不变。
- x86_64/aarch64/armv7 Linux musl 检查与 MIPS Nightly build-std release 构建通过；MIPS 仍不具备真机执行证据。
- 阶段 4 尚未结束。下一步是同步/发布/自更新的失败恢复与 staging/备份清理审计，以及批调用遇到执行状态不确定后是否应继续发后续命令的策略；阶段 5 发布脚本、产物和最终交付审计仍待完成。

## 阶段 4c 验证记录（2026-11-09）

- batch 对普通确定性远端错误继续执行独立检查；transport/protocol、unknown、manager_busy、response_limited 会停止后续项并报告 not_executed。结果顺序/数量不变，recovery.stopped_after 为 1-based 停止位置。故障注入测试确认首项已产生效果但响应丢失时，不重放、不建立下一条连接、不发送后续命令。
- sync_directory 在上传前验证完整 required_uploads 计划及目标绑定的随机 sibling staging 路径，manifest 按实际 RemoteRequest 编码预算预检。上传/提交状态不确定时不自动重连 abort，保留恢复状态；确定性的提交前失败才尝试清理，并通过 recovery 报告阶段、路径和清理确认。commit 失败保存 Agent 返回的恢复详情。
- Agent 提交时重新检查目标类型，拒绝把后来出现的普通文件挪作 backup；验证 staging 根/条目/marker 类型及 manifest 完整性。Unix 在 mode 应用与激活前 fsync 文件/目录，避免 mode=000 让校验失败；文件 mode 延后至校验后应用。成功结果 commit 区分 committed、durable、durability_error、metadata_cleaned；Windows 明确 durable=false，不能把缺少目录同步保证当作持久性已确认。失败恢复不再忽略 backup rename 错误。
- staging 清理先恢复根和父目录访问权限，再逐项检查普通目录，不通过 symlink chmod 外部路径。树遍历在入队时检查数量，marker 有界读取且拒绝特殊文件。新增目标类型变化、目录被换成 symlink、无访问权限 staging 清理、绝对/相对/Windows staging 路径边界回归。
- 发布事务把 spawn/cwd 等命令执行错误转成结构化步骤，start/health 失败也进入原子链接回滚；rollback_switch 与 rollback_start 失败返回 rollback_failed，rolled_back 表示旧链接是否确已恢复。current 被换成普通文件时不覆盖或删除；release 解析为绝对链接目标。Linux/macOS 回归覆盖 stop/start/health/rollback 程序不存在及 current 被替换的情况。
- 自更新增加 helper 最终回执，关联 attempt_id、候选 SHA-256、previous_instance_id。Agent 的 update_receipts 能力和 update.last_result，proxy 的 update_receipt/recovery 结果同步更新；proxy 等待 helper 稳定验证及 systemd 交还完成后才确认，不将临时候选上线或相同 Git revision 当作成功证据。pending/缺失/错配回执不确认成功，恢复失败保留 backup/manifest 并提供路径。
- helper 在旧进程退出后再次复核候选哈希，拒绝覆盖已有恢复路径。systemd 更新前验证 KillMode=process 与 Restart=on-failure；show/restart/stop/is-active 均限时，监督者缺失时不静默退化为无监督运行。更新 manifest 与回执分别限 64/16 KiB，回执原子落盘。
- 完整 workspace：Windows 163、Linux 165、Mac 163 项通过；三平台 Clippy 通过，Windows fmt/diff 检查通过。新增 `workflow_failures.rs` 覆盖 batch 断线与 busy/超大结果、普通错误继续、sync_commit 响应丢失后不重连清理。Windows 最终日志位于 `target/validation/tests-stage4c-final-windows.log`，Linux 为 `/tmp/remote-ops-stage4c-final.log`。
- `tools/verify_agent_update.py` 使用独立临时二进制/loopback 端口验证真实 helper：Windows 同二进制更新成功；Linux/macOS 同二进制更新成功和通过 self-check 后启动失败的候选自动回滚。Linux root 的 --systemd 测试使用唯一 transient unit，确认成功与回滚后 MainPID 都受原单元管理，测试后停止单元。无安装服务或常驻 Agent 被替换。
- Mac 最终源码快照 SHA-256 为 `c6c79f07773b43c7daf02579c75e3bfac878c874f8ed95dcc784619d4aaf1473`，通过原版 v3 连接上传到独立工作区后原生运行完整测试、Clippy、自更新成功/回滚实测。后台任务已关闭，常驻 Agent 仍为 PID 16096、protocol 3、原实例 ID。
- 三项 Linux musl 检查及 MIPS Nightly build-std release 通过；MIPS 仍缺真机执行证据。wire 保持 v4，变更为现有操作的结果元数据和内部 helper 文件格式。最终发布前必须明确版本/迁移约束，不能把原版 v3 或阶段早期 v4 的旧产物混入发布集合。
- 整体目标仍 active。下一阶段审计/重写 buildAll.ps1 的失败处理和产物收集，统一发布版本与支持矩阵，生成可复核产物并逐项完成最初目标的总验收。现有测试不构成发布和整体重构已完成的证据。

手动验证入口：

```sh
# 指向已授权的 Unix 测试 Agent 和专用工作目录，测试只在其下创建唯一子目录。
REMOTE_OPS_TEST_ADDR=IPv4:PORT REMOTE_OPS_TEST_ROOT=/absolute/test/workspace \
  cargo test -p remote-ops-proxy --test environment_contract live_unix_agent_round_trip -- --ignored --nocapture

# Linux root，BusyBox 和 Agent 都必须可在精简根目录中运行；proxy 在宿主运行。
python3 tools/verify_embedded_environment.py \
  --agent /absolute/static-agent --proxy /absolute/proxy --busybox /absolute/static-busybox

# 三平台独立更新进程；Linux/macOS 额外验证启动失败回滚。
python3 tools/verify_agent_update.py --agent /absolute/agent --proxy /absolute/proxy
# Linux root，可用 systemd 的测试机：独立 transient unit，退出后清理。
python3 tools/verify_agent_update.py --systemd --agent /absolute/agent --proxy /absolute/proxy
```

## 最终验收与 2.0 交付（2026-11-09）

本轮整体重构完成，配套版本为 2.0.0、远端协议 v4。前文的“尚未完成”是各阶段当时的状态；本节记录最终结论。未替换用户常驻服务。源码按用户要求提交推送后，从干净提交全平台重建，产物直接交付至 `Releases/` 根目录；不上传公开 Release。跨 v3 迁移步骤和行为差异见 [MIGRATING_V2.md](MIGRATING_V2.md)。

| 最初验收场景 | 实现与证据 |
| --- | --- |
| 1. 首次连接可决定下一步 | `environment` 返回实际 shell、init 观测、工具及有界检测状态；双端 environment_contract 和 PC→Mac 实测通过。完整 system_info 由 full 显式展开。 |
| 2. 按实际平台能力工作 | Windows、WSL Linux、Apple M1 Mac 原生运行测试；静态发布 Agent 在 BusyBox-only chroot 中无 procfs/systemd 运行，检测到 sh/BusyBox、未检测到 systemctl，不伪造 PID 1。 |
| 3. 明确命令语言 | Windows PowerShell 7/5.1/cmd/Git Bash 及 Unix 实际可用 shell 执行、cwd/env、引号、长脚本、退出码与输出 schema 回归通过；PC→Mac 验证 UTF-8 和显式解释器反馈。 |
| 4. 紧凑与完整反馈 | 全部 40 工具支持 response_mode；默认 compact 的省略分支显式报告，正文、错误、游标、截断和事务恢复信息保留；response_contract 和真实 stdio schema 验证通过。 |
| 5. 长任务和事务状态明确 | 连接接管/空闲、半帧与总预算、输出限制、任务增量读取与清理、不重放、batch 停止策略、同步提交断线、发布启动失败回滚均有真实双端或故障注入回归。三平台独立自更新和 Linux systemd 监督恢复实测通过。未对用户设备执行重启；重启确认/状态处理由回归验证。 |
| 6. 工具发现和契约一致 | 唯一注册表管理名称、注解、batch 策略；输入预检与共享协议参数双端校验；全部 40 工具有效/无效参数和输入/输出 schema 回归，README/AGENTS/迁移文档同步。 |
| 7. 可复核交付 | `targets/releases.json` 单一矩阵，11 目标、21 二进制全部 release 构建并通过 PE/Mach-O/ELF 架构、静态 musl、ARM 浮点及 MIPS32r2/o32/soft-float 检查。manifest/SHA256SUMS 及内嵌来源标记互相核对。 |

完整 workspace 测试：Windows 163、Linux 165、Mac 163 项通过，另有需显式指定远端的 live 测试默认 ignored，并已单独在 Mac 上运行通过。三平台 Clippy、fmt、diff 检查通过；发布构建脚本的 11 项安全测试三平台均通过。最终整理只改变验收文档和共享构建文件格式，发布来源标记随最终重建刷新。

发布构建入口为 `buildAll.ps1`/`tools/build_release.py`。实测拒绝了构建期间文档被修改的第一次 smoke build，未留下完成目录；单元测试覆盖 Cargo 报错、未报告 executable、旧来源、错误架构、动态 musl、源码变化和已有输出目录保护。成功矩阵先在 sibling staging 中收集，全部核验后才切换到输出目录；默认是 `Releases/v2.0.0`，本次最终收尾显式使用 `-Output Releases`。已有整个 Releases 目录先移入 `target/validation/` 中唯一命名的备份，不在原目录逐项覆盖旧产物，也不回退到旧 target 文件。

最终产物信息以 `Releases/manifest.json` 为准：记录版本、协议、唯一 build ID、源码 SHA-256 和逐文件哈希、Rust/Nightly/Zig 工具链、每个产物的目标/大小/SHA-256/内嵌 metadata；`Releases/SHA256SUMS` 同时覆盖 manifest。此前中间构建已保留在 `target/validation/release-matrix-trial-v2`，不能与最终目录混用。二进制和本地验收日志继续由 Git 忽略，提交只包含源码、测试与文档。

实机与环境范围：

| 目标或环境 | 验证范围 |
| --- | --- |
| Windows x64 | 原生完整测试、全部已发现 shell、Job Object、最终 gnullvm 发布文件自更新 |
| Linux x64（WSL Ubuntu） | 原生完整测试；静态 musl 发布文件的真实 MCP、自更新及 systemd 成功/回滚；BusyBox-only chroot |
| macOS ARM64（192.168.43.220） | 原生完整测试/Clippy/release；Windows 交叉构建产物的 self-check、PC→Mac 网络/补丁/传输、更新与回滚 |
| Windows ARM64、macOS x64、Linux ARMv7 两种 ABI/ARM64/RISC-V64GC/LoongArch64/MIPS32r2 | release 构建、格式/架构/ABI/来源与哈希校验；没有对应硬件的本轮执行证据 |

Mac 通过已注册原版 v3 RemoteOps 上传源码和候选文件，测试使用 `/tmp/remote-ops-v4.KFvEKInB` 及独立端口；所有测试 Agent 和后台任务都已停止/关闭。原版服务保持 PID 16096、实例 ID `1a5d0c512c94bc1d7de15cf3fb2cad8b`、protocol 3。源码快照 `source-stage5.tar.gz` SHA-256 为 `9b186bf6757895c2c508bdf1a0e45985d8f8807077943280299023314f0329f6`，对应最终整理前的完整回归源码。

主要本地日志：`target/validation/tests-stage5-windows.log`、`live-stage5-mac.log`、`live-stage5-cross-mac.log`、`stage5-windows-release-update.log`、`stage5-release-linux-update.log`、`stage5-release-embedded.log` 和 `release-matrix-v2.log`；Linux 完整回归为 `/tmp/remote-ops-stage5.log`，Mac 为测试工作区的 `stage5-mac.log`。

构建警告的实际边界：Windows 交叉构建 macOS 时没有本地 Xcode SDK，Rust 发出 SDK 发现警告；ARM64 产物已在用户 Mac 实际运行，x64 产物仍只有构建/格式证据。MIPS Tier 3 从源码构建标准库时报告不存在预编译 rustlib 目录，但构建和最终静态 ABI 校验成功；它不构成真机或 Linux 3.10.14 固件兼容性保证。没有引入 async runtime，保留单管理连接、串行 wire、固定连接值、二进制分帧和发送后状态不确定时禁止自动重放的架构边界。
