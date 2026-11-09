# 工具参考

RemoteOps 当前提供 40 个 MCP 工具。工具调用时的 schema 是准确参数形状的入口；本文说明选择、默认值和容易误用的行为。调用示例见[AI 使用指南](USAGE.md)，服务安装与升级见[运维指南](OPERATIONS.md)。

## 响应详细程度

`compact` 使用紧凑 JSON，并省略以下诊断分支；结果的 `_response.omitted_fields` 用 JSON Pointer 明确列出实际省略的字段：

| 工具 | compact 省略的分支 |
| --- | --- |
| `system_info` | `network`、`filesystems`；保留主机、CPU、内存、系统盘、身份、init、时间和工具链等信息。 |
| `agent_info` | `build`、`supported_operations`、`limits`、`update`；保留版本、协议、运行实例、平台和能力。 |
| `remote_probe`、`remote_status`、`set_remote` | 缓存的 `agent_info`（含 `last_probe.agent_info`）使用相同精简规则；保留连接状态、探测结果和错误。 |
| 其他工具 | 保留所有结果字段，JSON 文本使用紧凑格式。 |

`full` 返回全部原始结果字段和格式化 JSON。两种模式都保留文件正文、命令 stdout/stderr、退出码、超时、分页游标、截断标志和恢复信息。文本读取工具的正文与换行保持原样，不追加提示。MCP 同时提供文本回退和 `structuredContent`；精简规则同时作用于两者。响应选项只由 proxy 处理，不进入远端请求。

read_text/read_file_lines/tail_text 的正文位于文本 content，structuredContent 是读取元数据；其他成功工具的 structuredContent 包含结果对象。不要因为文本读取的 structuredContent 中没有 text 就判定文件为空。

例如，先用 `system_info {}` 获取日常快照；检查网络、挂载点或 inode 时使用 `system_info {"response_mode":"full"}`。需要自更新 staging 路径或完整能力限制时使用 `agent_info {"response_mode":"full"}`。`batch` 的子调用继承外层模式，也可以在各自的 `arguments` 中单独指定。

## 全部工具

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
| `pids` | `filter?`, `cursor?`, `limit?` | 有界进程分页；返回命令行可读/截断标志和扫描信息。过滤结果可为空但仍有继续游标。 |
| `process_info` | `pid` | Linux/Windows/macOS 进程详情和命令行可读/截断标志；Windows 的 `state`、`uid` 返回 `null`。`start_time_seconds` 为开机后秒数，`start_time_iso` 为启动墙钟时间（RFC 3339、Agent 本地时区）。 |
| `kill` | `pid`, `signal?` | Unix 发送数字信号；Windows 接受 9/15 并强制终止进程。默认 15。 |
| `pkill` | `name`, `signal?` | 按平台进程名完整匹配（Windows 不区分大小写）并排除 agent 自身，默认 signal 15；Linux/macOS 名称分别最多 15/31 字节，Windows 最多 260 个 UTF-16 单元且 signal 仅接受 9/15。匹配超过 1024 个进程时不执行，返回 `matched`、`signaled_pids` 和 `failed_pids`。 |
| `sh_exec` | `command`, `shell?`, `cwd?`, `env?`, `timeout_ms?` | 显式选择 sh/bash/BusyBox/PowerShell 7/5.1/cmd/Git Bash；默认 native，结果返回实际 shell 路径。最长 300 秒。 |
| `exec` | `program`, `args?`, `cwd?`, `env?`, `timeout_ms?` | 不经过 shell 执行程序。结果含 `duration_ms`。 |
| `process_start` | `program`, `args?`, `cwd?`, `env?`, `timeout_ms?` | 启动由 agent 管理的后台程序并立即返回 job ID；默认最长 1 小时，最大 24 小时。 |
| `process_output` | `job_id`, `stdout_cursor?`, `stderr_cursor?`, `max_bytes?` | 按绝对字节游标增量读取后台任务的 stdout/stderr。 |
| `process_wait` | `job_id`, `wait_ms?` | 有界等待后台任务退出，默认 10 秒，最长 30 秒。 |
| `process_signal` | `job_id`, `signal?` | 向 Unix 任务进程组发送信号；Windows 接受 9/15 并终止整个 Job Object。 |
| `process_close` | `job_id` | 释放已结束任务及其保留输出；运行中的任务必须先 signal 并 wait。 |
| `system_info` | `response_mode?` | 有界读取系统、CPU、身份权限、时间、init 和工具链画像；`full` 另返回网络与文件系统详情。 |
| `environment` | 无必填参数 | 不运行诊断命令，发现实际 shell、native 选择、PID 1、服务管理程序、cwd 和执行能力；区分“检测到文件”和“执行验证”。 |
| `upload_file` | `local_path`, `remote_path`, `overwrite?`, `mode?`, `resume?` | 从 proxy 所在 PC 上传一个普通文件，可设置 Unix mode 并校验续传。 |
| `download_file` | `remote_path`, `local_path`, `overwrite?`, `resume?` | 下载一个普通文件到 proxy 所在 PC，可校验续传。 |
| `remote_status` | 无 | 被动查询地址、缓存连接、生命周期状态，以及最近一次成功、错误、探测和 Agent 信息，不主动连接。 |
| `set_remote` | `ip?`, `port?` | 动态设置远端 IPv4 或端口；至少提供一项，未提供部分保持不变。总是丢弃缓存会话（即使地址未变化），使下一次调用重新建立连接。 |
| `agent_info` | `response_mode?` | 查询版本、协议、运行实例、平台和能力；`full` 另返回构建、操作清单、限制和更新路径。 |
| `remote_probe` | `timeout_ms?` | 主动连接或健康检查远端，返回可达性、延迟、Agent 信息或结构化错误。 |
| `wait_remote` | `wait_for?`, `timeout_ms?`, `poll_interval_ms?`, `probe_timeout_ms?` | 有界轮询远端，等待 `online`、`offline` 或 `offline_then_online`。 |
| `reboot` | `delay_ms?` | 请求 Agent 延迟重启设备，并将 proxy 生命周期状态切换为 `rebooting`。 |
| `agent_update` | `local_path`, `timeout_ms?`, `poll_interval_ms?`, `probe_timeout_ms?` | 上传并验证 Agent 候选程序，原子替换、重启验证，失败时自动回滚。 |
| `batch` | `calls`, `response_mode?` | 最多 16 项，先完整预检再顺序执行。普通远端错误逐项返回；遇到连接/协议不确定、manager_busy 或结果超限则停止后续项并标记 not_executed。命令可修改状态，不提供事务或回滚。 |

## 路径、文本与文件传输

- `local_path` 在 proxy 电脑；remote_path/path 等指向设备。全部文件操作按精确路径执行，不把通配符当删除目标。目录查询/搜索使用独立 pattern/glob 参数。
- 文本读取、file_hash、上传和下载只接受普通文件，拒绝目录、symlink 和特殊文件。stat 不跟随 symlink，list_files 可列出它但不递归进入；递归 copy 拒绝 symlink，递归 remove 不跟随它。
- `read_file_lines` 从第 1 行计数，end_line 包含在范围内；默认读取 200 行，每次最多 10,000 行、1 MiB，为定位起始行最多扫描 64 MiB。返回 next_line 时继续，不按字符数计算行偏移。
- `read_text` 每次最多返回 1 MiB，offset/next_offset 使用字节。`tail_text` 最多 10,000 行、1 MiB，保留原行尾和末尾换行；读取工具不向正文添加截断标记。
- `list_files` 默认 limit=200，最大 1,000；recursive=false，max_depth 默认 16、最大 64；最多扫描 100,000 项、输出 1 MiB。递归 name 是 `/` 分隔相对路径，pattern 最多 1 KiB。
- `grep` 使用 Rust 正则，case_sensitive=true；pattern 最多 4 KiB，glob 最多 1 KiB。max_results 默认 200、最大 1,000，max_file_bytes 默认 1 MiB、最大 16 MiB。每次最多枚举 100,000 项、扫描 10,000 文件或 64 MiB、递归 64 层、输出 1 MiB；每条匹配正文最多 1 KiB。跳过 `.git`、`.hg`、`.svn`、`.next`、`node_modules`、`target`、`dist`、`build`。
- `apply_patch` 只更新一个已存在的普通 UTF-8 文件；补丁路径须与 path 完全一致，旧上下文须唯一匹配。补丁最多 256 KiB、128 个 hunk，目标最多 16 MiB；保留 BOM、原行尾和末尾换行状态，不支持创建、删除、重命名或无上下文纯插入。
- upload/download 的 overwrite 默认 true，resume 默认 false，单文件默认上限 4 GiB；续传校验已有长度和前缀哈希，完成时校验全长/完整 SHA-256。错误下载前缀回退为 0。resume=true 时保留可恢复 partial，正常目标在校验完成前不出现半文件。
- mkdir/copy/move/remove 的递归或覆盖选项必须按需显式设置；目录覆盖始终拒绝，move 不隐式 copy-delete 跨文件系统。Windows move/symlink 的 atomic=false，不作 Unix 原子性承诺。Unix mode 范围为 0..=4095。

补丁示例（下方路径必须换成请求中的实际 path）：

```text
*** Begin Patch
*** Update File: /srv/app/config.txt
@@
-enabled=false
+enabled=true
 name=app
*** End Patch
```

## 命令与进程

| 项目 | 默认 / 上限与结果 |
| --- | --- |
| 前台 exec/sh_exec | 超时最大 300 秒；stdout/stderr 各保留最多 256 KiB，返回 duration_ms、退出码、超时和截断状态 |
| sh_exec 脚本与上下文 | command 最多 256 KiB UTF-8；cmd 另限 8,000 UTF-16 单元；cwd 最多 4 KiB；env 最多 128 项、合计 64 KiB |
| 后台任务 | 默认超时 1 小时、最大 24 小时；同时保留 16 个，全部运行时拒绝新任务，否则先回收最早结束项 |
| process_output | 每路默认 64 KiB、最大 256 KiB；使用绝对字节游标，各路只保留最近 256 KiB |
| process_wait | 默认 10 秒，最长 30 秒；超时只是等待结束，后台任务继续受自身 timeout 管理 |
| pids | limit 默认/最大 1,024；每页最多检查 4,096 个进程、读取 16 MiB 命令行、输出 1 MiB；PID 快照最多 65,536 项 |
| 命令行可用性 | pids/process_info 最多保留 128 KiB 前缀，以 cmdline_available/cmdline_truncated 区分不可读和截断；scan.filter_incomplete 表示过滤可能不完整 |
| kill/pkill/process_signal | Unix 数字信号；Windows 仅 9 或 15，均强制终止。process_signal 操作整个进程组/Job Object，kill 按 PID |

后台任务不跨 Agent 重启持久化，close 只释放已结束任务。pkill 完整匹配平台进程名，排除 Agent 自身；Linux/macOS 名称最多 15/31 字节，Windows 最多 260 UTF-16 单元且不区分大小写。匹配超过 1,024 项不发送任何信号。

native 在 Unix 使用 sh，在 Windows 优先 pwsh 再选 powershell。sh/bash/busybox 仅 Unix；powershell/cmd/git_bash 仅 Windows；pwsh 在检测到它的 Unix/Windows 均可用。显式选择不可用 shell 返回 unsupported，不跨语言回退。

## 同步与发布

sync_directory 默认最多 4,096 个 manifest 条目、4 GiB、32 层；硬上限 10,000 项、4 GiB、64 层。excludes 最多 64 个 glob，每个最多 1 KiB。本地拒绝 symlink、特殊文件与非 UTF-8 相对路径；远端支持时保留 Unix mode。

返回 files_transferred/files_reused 和字节统计，仅变化普通文件经传输。完整校验后切换目录，旧目录保存到 backup_path，不自动删除旧备份。committed 是切换确认，commit.durable 是持久性确认；不能互相替代。状态不确定时保留 recovery，不自动重连清理。

deploy_release 仅 Unix；release_id 限 1..=128 个 ASCII 字母、数字、`.`、`_`、`-`。release 是 releases_path 的直接子目录，current_path 不存在或为 symlink；检查架构、空间、父目录写权限及最多 64 个依赖。

start/health 必填，stop 可选，rollback_start 默认使用 start；全部是不经过 shell 的 exec 命令对象，每步最多 300 秒。启动或健康失败时同一远端请求内回滚链接和服务，status 为 deployed、stop_failed、rolled_back 或 rollback_failed。若需要真实守护进程，应由这些命令操作已有服务或启动脚本，前台命令的超时不会被当作后台服务部署成功。

## 系统与生命周期

- system_info 的 Linux 集合有界：网卡 128、地址 512、路由 256、监听端口 512、mount 256、工具链 24。available/truncated 区分采集可用性和截断；read_only 同时检查挂载点及 superblock。其他平台仅返回可靠可采集的值，不将缺失值当作已确认没有资源。
- environment 最多搜索 128 个绝对 PATH 目录，忽略相对路径、非 UTF-8 路径及超过 4 KiB 的条目，报告搜索是否完整。检测到 shell、BusyBox 或 systemctl 文件不证明实际可用。
- remote_status 被动返回缓存，cached 不保证在线；set_remote 至少指定 IP 或 port，未指定部分保留。remote_probe 默认 5 秒，范围 100..=30,000 ms。
- wait_remote 支持 online/offline/offline_then_online。正常默认 online，重启/更新时默认 offline_then_online；总超时默认 120 秒，范围 1..=600,000 ms，轮询默认 1 秒、范围 100..=10,000 ms，probe_timeout 默认 5 秒、范围 100..=30,000 ms。离线后再在线或实例 ID 变化均可确认切换。
- reboot 延迟默认 1 秒、范围 250..=10,000 ms；Linux 要 root，Windows 使用系统重启程序，macOS unsupported。acknowledged/disconnect_observed 区分确认与实际断开，不把普通超时等同于重启成功。
- agent_update 的等待/探测参数沿用 wait_remote；结果 status 为 updated/rolled_back/timed_out/unconfirmed。确认成功必须检查关联本次尝试的最终回执。安装路径和 last_result 见 Agent 信息 full；操作步骤见[运维指南](OPERATIONS.md#更新-agent)。

## 错误结果

详情为 `{kind,message,source,outcome,request_started,prior_requests,recovery?}`。source 区分 proxy/remote/transport/protocol，request_started 只表示已开始发送尝试；prior_requests 是本次编排里最后失败请求之前的请求数。recovery 可包含失败阶段、路径、清理确认与 batch 停止位置。

发送前参数错误是 JSON-RPC -32602，详情在 error.data；执行错误是 isError=true，文本 content 为 JSON `{error:...}`，不强行套用成功 outputSchema。batch 每项为 `{tool,ok,result|error}`。

outcome 包含 not_sent、unknown、remote_error、rejected、response_limited、local_error。[AI 使用指南](USAGE.md#失败后先判定是否执行)给出决策顺序。JSON 编码也有上限，控制字符可能使返回片段早于原始字节上限结束；按返回游标和截断标志继续。response_too_large 不代表命令未执行。

更底层的帧、stdio、资源预算及事务实现要求见[实现契约](CONTRACTS.md)，这些限制不改变用户应通过工具 schema 传递参数的方式。
