# 2.0 重构与验证记录

本页保留影响维护的结论和验证范围；当前行为以[实现契约](CONTRACTS.md)为准，复现命令见[构建与验证](BUILDING.md)。不再按每轮修改重复列举同一套通过记录。

## 解决的问题

| 原有问题 | 2.x 处理 |
| --- | --- |
| 大型诊断快照消耗上下文，日常调用无法按需精简 | proxy 边界提供 compact/full，明确报告省略字段，保留正文、游标、错误和恢复信息 |
| Windows 依赖固定 Git Bash，Linux 命令依赖被默认猜测 | environment 发现实际 shell/init/工具；sh_exec 显式 shell/cwd/env，不按平台猜嵌入式或 systemd |
| MCP 入口约 3,800 行，注册/schema/校验分散 | 拆为入口、唯一 catalog、input/output、parameters、dispatch、batch；全部工具严格预检 |
| batch 后项参数报错前，前项命令可能已执行 | 全批预检后才执行首项；不确定错误停止后项并标记未执行 |
| 普通远端报错和传输状态不确定混在一起 | 分离 source/outcome，确定错误可保留同步连接，unknown 不自动重放 |
| 慢握手/半帧、编码放大、进程采集可能突破预算 | 共用绝对截止时间、有界编码/stdio/采集、分页与明确截断 |
| 同步、发布失败时恢复上下文不足 | staged tree 复核、durable 区分、未确认提交不自动 abort、启动错误也进入回滚 |
| 自更新短暂上线或相同 revision 可能被误认完成 | 唯一 attempt + 候选哈希 + 前实例关联最终回执；systemd 监督权恢复与稳定性检查 |
| 发布脚本失败后打印成功、收集固定目录的旧文件 | Cargo 报告产物 + 内嵌来源 + 架构/ABI 校验 + 全新目录交付 |

2.0 引入远端协议 v4，v3 不兼容；工具稳定为 40 个。2.0.1 保持 v4，修复 Linux superblock 只读识别和 zombie 退出检测，详见[MIPS 记录](MIPS_VALIDATION.md)及[迁移说明](MIGRATING_V2.md)。

## 保留的设计边界

同步实现、单活动管理者、串行请求、固定内置连接值、有界资源、二进制文件分帧和发送后不重放仍是设计约束。响应精简只发生在 MCP 呈现层，不改变传输完整性或生命周期决策。

环境发现是文件和平台观测，不是“可用性认证”。没有工具、权限、procfs 或服务管理器时如实报告；BusyBox applet、命令实际输出与服务状态需执行验证。

## 已完成的验证（2026-11-09）

| 范围 | 证据与边界 |
| --- | --- |
| 2.0 重构最终 workspace | Windows 163、Linux 165、macOS ARM64 163 项测试；三平台 Clippy。后续 2.0.1 Linux 增至 167 项，Windows 163 项。macOS 数字属于 2.0 阶段，不能当作每次新提交的原生回归。 |
| 实际 MCP 工具 | Mac、WSL Linux、Windows 各调用覆盖 40 工具，分别 67、67、91 项断言。MIPS 真机随后 72 项通过。平台 unsupported 与权限拒绝单独计入边界。 |
| 二进制、任务与事务 | 跨 Chunk 往返、前缀续传/回退、仅传变化同步、Unix 发布健康失败回滚、后台断线查询/游标/输出窗口、命令超时清理 |
| 自更新 | 临时副本成功/候选启动失败回滚；Linux systemd 独立单元交还监督；2.0.1 额外保留旧 zombie 到更新成功后才回收 |
| 精简环境 | Linux 静态 Agent + BusyBox-only chroot，无 procfs/systemd；实际设备 BusyBox 1.22.1 applet 差异另测 |
| 发布 | 11 目标、21 文件；格式、静态 musl/浮点/MIPS ABI、源码和构建 ID、SHA256SUMS；发布器 12 项安全回归 |

未对用户主机做成功 reboot 测试。macOS unsupported、非 root Linux 权限拒绝以及参数拒绝已验证；这些结果不能代替成功重启的运行证据。

Windows/macOS x64/ARM64、Linux 各架构的构建矩阵不等于全硬件运行矩阵。Windows x64、Linux x64、macOS ARM64 和 MIPS 有实际执行记录；其他目标主要是交叉构建与产物校验证据。MIPS 实机内核 4.4.94，不承诺其他固件内核兼容。

## 如何继续验证

以工作区测试和统一发布脚本复现，不依赖历史目录里存在同名二进制。真实远端测试需明确地址、独立目录和运行权限；自更新脚本用临时副本，不默认替换安装服务。

临时日志、来源快照和机器可读用例报告保存在忽略的 `target/validation/`，不作为读者必须拥有的仓库文件。涉及当前安装或发布来源，读取实际 `agent_info`、最终回执和对应 manifest，不从本页推断运行状态。
