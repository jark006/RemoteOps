# 升级与版本迁移

当前版本系列为 2.x，远端协议 v4。常规更新步骤见[运维指南](OPERATIONS.md#更新-agent)。升级前保留旧程序、服务配置和可用维护入口。

## 先判断升级方式

| 当前设备 | 升级方式 |
| --- | --- |
| 1.x / 协议 v3 | 手动切换 Agent，再换配套 proxy；新版 proxy 无法连旧协议编排升级 |
| 早期 v4 测试版，无 update_receipts | 手动安装正式配套版本 |
| 正式 2.0.0 / v4 | 可用 agent_update；Linux 首次更新先检查下文的父进程回收条件 |
| 2.0.1 / v4 | 按常规自更新流程，匹配 build.target 并检查最终回执 |

程序版本号、协议与能力以实际 `agent_info` full 或设备上的 `--self-check` 为准；不靠文件名判断。

## 从 v3 手动迁移

1. 用 SSH/SCP 或仍可连接旧 Agent 的旧版 proxy，将新版 Agent 上传到独立路径。按设备架构选择文件，设置执行权限并运行 --self-check。
2. 在独立端口试运行候选，用新版 proxy 的 --remote 连接，验证 remote_probe、environment 和业务所需工具。
3. 在维护窗口停止旧服务，备份并切换实际安装路径，启动原服务。systemd 使用仓库单元的 KillMode=process、Restart=on-failure；不要让临时 Agent 与服务抢同一端口。
4. 替换电脑上的 proxy，重新加载 MCP 客户端，确认版本与设备状态。失败时恢复旧程序和配套旧 proxy。

v4 不兼容降级连接 v3。不能先换掉唯一可用的旧 proxy，再指望新版 agent_update 上传跨协议候选。

## 2.0.0 的首次 Linux 自更新

2.0.0 helper 用 kill(pid,0) 判断旧 Agent 是否退出。若它由停在输入状态的 BusyBox 交互式 shell 启动，退出后可能保持 zombie，导致 helper 超时退出、Agent 端口不再监听。首次升级执行的仍是旧 helper，新候选包含修复也不能提前改变它。

迁移前确认 Agent 由可回收子进程的启动者运行，例如合规 systemd 单元，或运维指南中的 `agent & child=$!; wait "$child"` 包装 shell。先结束已有后台任务，保留其他维护入口。

如果出现 status=timed_out/unconfirmed，回执 detail 为旧 PID 未退出：

1. 从维护终端检查回执、manifest、进程状态和候选/目标，确认旧进程是否为 Z；不要直接重复 agent_update。
2. 若目标仍是旧程序且 helper 已退出，可恢复旧 Agent 并使启动者正确 wait，再核实恢复路径后重试迁移。未确认的恢复文件先保留。
3. 2.0.1 helper 会有界识别 Linux Z/X/x，后续更新不再被这种未回收 PID 阻塞；procfs 不可读时保守退回存活探测。

实际问题和验证记录见[MIPS 真机验证](MIPS_VALIDATION.md)。

## 调用方需要调整的行为

| 范围 | 2.x 行为与调整 |
| --- | --- |
| 反馈 | 默认 compact；需要网络/文件系统、build/limits/update 等分支时传 response_mode=full，省略字段见 _response.omitted_fields |
| Windows 脚本 | native 优先 PowerShell 7 再选 5.1；旧 Bash 脚本显式指定 git_bash，cmd 脚本指定 cmd |
| Unix 环境 | 先查 environment，再决定 sh/bash/BusyBox、服务管理方式；文件存在不证明实际可运行 |
| 进程枚举 | 跟随 pids.next_cursor，检查 cmdline_available/cmdline_truncated 和 scan.filter_incomplete |
| 错误恢复 | 按 source/outcome/request_started/prior_requests/recovery 处理；unknown 不能自动重放 |
| batch | 全部预检后顺序执行，检查每项 ok/error；不确定错误后项为 not_executed，命令仍可修改状态 |
| 同步/发布/更新 | 检查 committed/durable、rolled_back/rollback_failed、以及关联本次更新的最终回执 |

2.0.1 修复 Linux 只读 superblock 识别和 zombie 等待，不改变工具输入、结果字段或 v4 wire 格式。工具详情见[工具参考](TOOLS.md)，升级产物核对见[运维指南](OPERATIONS.md#选择下载文件)。
