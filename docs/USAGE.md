# AI 使用指南

本文供通过 RemoteOps 操作设备的 AI Agent 使用。安装见[README](../README.md)，准确参数形状读取当前会话的工具 schema，完整行为查[工具参考](TOOLS.md)。以下写法是“工具名 + 参数对象”，不是需要用户在终端输入的命令。

## 建立工作上下文

1. 确认用户指定的设备、端口、工作目录和允许的操作。没有明确的修改任务时先读取信息。
2. 用 `set_remote` 设置目标，再调用 `remote_probe` 确认在线。只有 `remote_status` 的缓存状态不能证明可达。
3. 调用 `environment`，记录平台、默认 shell、实际解释器路径、init 观测、工具清单和搜索是否完整。
4. 按任务补信息：日常看 `system_info {}`；需要网络、挂载点或 inode 时用 full。更新前获取 `agent_info {"response_mode":"full"}`。

```text
set_remote {"ip":"192.168.1.50","port":8022}
remote_probe {"timeout_ms":5000}
environment {}
```

`set_remote` 只改变当前 proxy 进程，且总会丢弃缓存会话，即使地址没有变化。新会话应重新确认目标；不要依赖上一个会话曾设定的 IP。

## 按任务选择工具

| 任务 | 首选 |
| --- | --- |
| 找文件、定位文本 | `list_files` + pattern，`grep` + glob |
| 查看局部内容 | `read_file_lines`；尾部日志用 `tail_text` |
| 读取有界文本前缀 | `read_text`，按 next_offset 继续 |
| 修改现有文件的一小部分 | 先读文件/哈希，再 `apply_patch` + expected_sha256 |
| 写入完整文本 | `write_text` |
| 传输二进制 | `upload_file` / `download_file`，不要放进命令或 JSON 正文 |
| 同步整棵目录 | `sync_directory`；需要启动、健康检查、回滚时用 `deploy_release` |
| 执行已知程序及参数 | `exec`；只有需要脚本语法时用 `sh_exec` |
| 长任务或需要随时查询输出 | `process_start` → `process_output` / `process_wait` → `process_close` |
| 一组独立检查 | `batch`；先查允许的工具，命令仍可能修改状态 |

文件工具中的远端路径属于设备。`upload_file`、`download_file`、`sync_directory`、`deploy_release` 和 `agent_update` 的 `local_path` 属于运行 proxy 的电脑，不属于设备，也不一定属于 AI 的其他执行环境。

传输示例显式拒绝覆盖已有文件；目录同步按需保留返回的备份路径：

```text
upload_file {"local_path":"D:\\deploy\\app.bin","remote_path":"/tmp/app.bin","overwrite":false,"resume":true}
download_file {"remote_path":"/tmp/result.bin","local_path":"D:\\results\\result.bin","overwrite":false,"resume":true}
sync_directory {"local_path":"D:\\deploy\\app","remote_path":"/srv/app-next","excludes":["*.log"]}
```

检查传输返回的大小和哈希，按任务核对目标内容；不要把上传完成等同于程序已部署或启动。同步是目录切换，会保留旧树，需要确认服务行为时使用发布工具。

多项检查可以组合，并给某个子调用单独选择完整反馈：

```text
batch {"calls":[{"tool":"environment"},{"tool":"agent_info","arguments":{"response_mode":"full"}},{"tool":"stat","arguments":{"path":"/srv/app-next"}}]}
```

## 选择命令语言

`environment.shells.detection=file_metadata` 只说明发现了候选文件。程序能否运行、BusyBox 是否包含所需 applet、服务管理器是否可用及当前权限，都要由实际执行结果确认。

```text
sh_exec {"shell":"busybox","command":"printf '%s\n' \"$DEVICE\"; pwd","cwd":"/tmp","env":{"DEVICE":"test-device"}}
sh_exec {"shell":"pwsh","command":"Get-Location; $PSVersionTable.PSVersion","cwd":"C:\\work"}
exec {"program":"/usr/bin/git","args":["status","--short"],"cwd":"/srv/app"}
```

- Unix 的 native 使用 sh；需要 Bash 语法时显式选 bash。设备可能只有 BusyBox，也可能某些 applet 没有编译进去。
- Windows 的 native 优先 PowerShell 7，再选 5.1。cmd 和 Git Bash 必须显式选择；不把脚本交给另一种语言“碰碰运气”。
- 返回的 `shell.name/program` 是实际解释器。检查 stdout/stderr、exit_code、timed_out 和截断标志，不能只看工具调用是否成功返回。
- 原生程序/cmd 输出按有损 UTF-8 解码。已知程序默认使用系统代码页时，使用它自身的 UTF-8 选项；例如 Python 可通过 env 设置 `PYTHONIOENCODING=utf-8`。
- 发现 systemctl 不等于设备正在由 systemd 管理。Linux 的 PID 1 观测、实际服务状态和权限应共同判断。

## 控制上下文与分页

默认 compact 可用于日常工作；full 用于补齐被明确省略的诊断字段。`_response.omitted_fields` 表示省略，`truncated` 表示数据达到上限，两者含义不同。

- 目录与进程枚举跟随非空 next_cursor，即使当前页没有结果。给搜索加目录和 glob 范围，避免无目的扫描整机。
- 文本按 next_offset 或 next_line 继续。不要自行按返回字符数计算字节偏移。
- `pids.scan.filter_incomplete=true` 时，空结果不能证明进程不存在。
- 后台输出保存最近一段数据。cursor 落后时先记录丢失范围，从返回的最早位置继续；stdout/stderr 游标分别维护。

## 长任务要有结束动作

```text
process_start {"program":"/usr/bin/make","args":["all"],"cwd":"/srv/app","timeout_ms":3600000}
process_output {"job_id":1,"stdout_cursor":0,"stderr_cursor":0}
process_wait {"job_id":1,"wait_ms":10000}
process_close {"job_id":1}
```

用真实返回的 job_id 和 next_*_cursor 替换示例值，循环读取和等待，确认 state 已结束后才 close。任务不属于连接，断开重连后仍可查询；Agent 重启后不保留。要取消运行中的任务，先 `process_signal`，再 wait、读取剩余输出并 close。不要为了释放任务直接关闭仍在运行的 job。

## 失败后先判定是否执行

| 结果 | 下一步 |
| --- | --- |
| not_sent | 当前请求未发送；检查参数、PC 路径或连接。编排的 prior_requests 仍可能已有作用。 |
| rejected / manager_busy | 候选请求未执行；已有管理者正在工作，稍后再检查。 |
| remote_error | 远端明确报错；读取详情及 recovery，不假设之前步骤无副作用。 |
| unknown | 请求发出后失联；先核对文件、进程、链接或服务状态，禁止自动重放。 |
| response_limited | 操作可能已经完成，结果超限；缩小后续读取，不能直接重跑命令。 |
| local_error | 检查 PC 端文件提交状态，保留可恢复内容。 |

错误可能位于 `isError:true` 的文本 JSON `{error:...}`、JSON-RPC error.data，或 batch 子结果。不要假定所有错误都有成功时的 structuredContent。

batch 全部参数先预检，再顺序执行；普通远端错误后仍可继续独立检查，连接/协议不确定等错误会使后项标记 not_executed。逐项读取 ok/error，不把 batch 当事务或一次远端请求。

## 修改与生命周期收尾

- 删除、移动、终止进程只使用已核实的精确路径/PID/进程名。做测试时创建独立目录和进程，记录清理结果。
- 同步结果 committed 和 durable 分别表示切换与持久性确认。Windows 的 durable=false 需如实报告；不要自动删除返回的旧 backup。
- Unix 发布要检查 status。rolled_back 与 rollback_failed 不能混用；启动/健康失败后检查旧链接和旧服务是否恢复。
- 设备重启、服务切换、自更新在用户授权范围内执行。等待 online 只说明当前可达；确认重启用 offline_then_online 或实例变化。
- 自更新只有与本次候选哈希、previous_instance_id、attempt_id 对应的最终回执才能确认成功。timed_out/unconfirmed 时保留 recovery 路径，检查实际文件与进程，不盲目重复上传。

服务安装、自更新准备和跨版本升级步骤见[运维指南](OPERATIONS.md)与[迁移指南](MIGRATING_V2.md)。
