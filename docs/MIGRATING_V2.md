# 从 1.x 迁移到 2.0

当前版本为 2.0.1，远端协议保持 v4；已有 2.0.0 且支持 update_receipts 的 Agent 可直接通过 agent_update 升级。2.0.1 修复 Linux mountinfo 的 superblock 只读选项识别，不改变工具输入、输出字段或 wire 格式。

2.0.1 同时修复 Linux 更新 helper 等待未回收 zombie 进程超时的问题。首次从 2.0.0 升级仍使用旧 helper：若 Agent 是交互式 shell 的前台任务，shell 停在输入状态时可能不及时 reap，需先确保由能 wait/reap 的启动者运行（例如 systemd，或后台包装 shell 执行 `agent & wait $!`）。若回执为 unconfirmed，detail 指出旧 PID 未退出且进程实际是 Z，检查恢复路径并通过现有维护入口重启旧 Agent 后再完成迁移；不要把超时当作候选已安装，也不要盲目重复更新。2.0.1 helper 在有界 procfs 读取中将 Z/X/x 识别为已退出，procfs 不可读取时仍保守探测。

2.0 的 proxy 与 Agent 使用远端协议 v4，拒绝连接 v3。文件内容仍使用二进制 Chunk，不进入 MCP JSON。固定连接值、单管理连接和请求不自动重放的边界保持一致。

## 安装顺序

1. 保留可用的旧版 proxy/Agent 及服务配置。用旧版 proxy 上传 2.0 Agent 到独立路径，不覆盖正在运行的程序。
2. 先用独立监听端口启动新 Agent；使用 2.0 proxy 的 `--remote IPv4:PORT` 验证 `remote_probe`、`environment` 和所需工具。
3. 在维护窗口切换服务使用的 Agent，再切换 MCP 客户端配置中的 proxy 路径并重启客户端。Linux systemd 单元沿用 `deploy/remote-ops-agent.service` 的 `KillMode=process`、`Restart=on-failure`。
4. 确认连接和任务正常后再停用旧服务。升级失败时恢复原服务与配套旧 proxy。

跨 v3→v4 不能由新版 `agent_update` 编排：握手会先拒绝旧协议。后续 v4 内更新还要求 Agent 报告 `update_receipts`；阶段早期测试版 v4 未提供该能力时，也需要先手动切换到正式 2.0。不要以候选进程短暂上线作为更新成功证据，应检查关联本次尝试的最终回执。

## 调用变化

| 范围 | 2.0 行为 | 调用方调整 |
| --- | --- | --- |
| 响应 | 默认 compact；省略字段见 `_response.omitted_fields` | 依赖完整诊断分支时传 `response_mode: "full"`，或 proxy 启动参数 `--response-mode full` |
| Windows `sh_exec` | native 优先 PowerShell 7，再选 5.1 | 既有 Bash 脚本显式传 `shell: "git_bash"`；cmd 脚本传 `shell: "cmd"` |
| Unix `sh_exec` | native 使用 sh，可显式选 bash/BusyBox/pwsh | 先查询 `environment`，不要按 Linux 名称猜测 shell、systemd 或 BusyBox applet |
| shell 运行上下文 | 支持 cwd/env；返回实际解释器 | 使用结构化参数传上下文，长任务使用 `process_start` |
| `pids` | 有界分页、扫描及命令行可用性元数据 | 沿 next_cursor 继续；filter_incomplete 时不把空结果当作确定无匹配 |
| 错误 | source/outcome/request_started/prior_requests，必要时 recovery | outcome=unknown 的操作不能自动重放，先检查远端状态和恢复路径 |
| `batch` | 全部预检后顺序执行，错误后可能跳过后项 | 查看每项 ok/error；遇状态不确定、busy、协议或输出上限错误后，后项为 not_executed |
| 更新/发布/同步 | 结构化事务状态、回滚与持久性信息 | 区分 committed/durable、rolled_back/rollback_failed，保留未确认的恢复文件供检查 |

`batch` 的 exec/sh_exec 仍能产生副作用，“诊断工具集合”不保证调用只读。工具发现使用统一注册表并提供注解；真正的平台支持和可执行程序以 Agent 返回的信息及实际调用结果为准。

## 核对发布文件

本次最终交付直接位于 `Releases/` 根目录，使用 `Releases/manifest.json` 和 `Releases/SHA256SUMS` 核对大小、哈希、目标和构建来源。构建器默认的版本子目录布局对应 `Releases/v2.0.1/manifest.json`，以实际输出目录为准。每次构建有独立 build ID，不与其他目录中的旧产物混用。MIPS 文件为小端 MIPS32r2/o32/soft-float 静态 musl；仍需在具体设备上验证内核、指令与固件兼容性。编译和 ABI 校验不能替代真机运行。
