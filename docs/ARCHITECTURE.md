# 架构与代码导航

本文解释模块责任和修改入口。具体边界见[实现契约](CONTRACTS.md)，构建与验证见[BUILDING](BUILDING.md)。当前版本从 workspace 读取，远端协议为 v4。

## 两个进程与两种接口

```mermaid
flowchart LR
    Client[AI 客户端] <-->|MCP stdio| Proxy[PC proxy]
    Proxy <-->|认证 TCP：控制帧与二进制 Chunk| Agent[远端 Agent]
    Proxy --> Local[PC 文件系统]
    Agent --> Remote[远端文件、进程与平台接口]
```

MCP JSON-RPC 只存在于 proxy 的 stdio 侧。proxy 主动连接一个 Agent；文件字节走二进制帧，PC 文件读写也由 proxy 负责，因此 local_path 属于 proxy 主机。所有日志写 stderr。

Agent 同一时刻只有一个活动管理者，请求串行处理。认证后的空闲连接可由新管理者接管；旧请求从开始到完整响应发送都处于 busy，候选首请求被拒绝为 manager_busy，保证不执行。连接和 handler 有界；不引入 async runtime、请求多路复用或失败自动重放。

## 模块责任

| 路径 | 责任 / 改动入口 |
| --- | --- |
| `crates/protocol/src/lib.rs` | 协议版本、握手、帧、会话及 wire 数据类型 |
| `crates/protocol/src/arguments.rs`、`execution.rs` | 双端共享参数、限额和校验 |
| `crates/protocol/src/encoding.rs` | 有界编码 |
| `crates/proxy/src/stdio.rs`、`mcp.rs` | 有界 stdio 与 JSON-RPC 分发 |
| `crates/proxy/src/mcp/catalog.rs` | 工具身份、注解、batch 资格的唯一注册表 |
| `mcp/input.rs`、`output.rs` | 工具 schema |
| `mcp/parameters.rs`、`dispatch.rs`、`batch.rs` | 参数预检、已预检调用执行、整批预检和有序执行 |
| `crates/proxy/src/client.rs`、`client/error.rs` | 会话、传输、探测、生命周期编排与结构化错误 |
| `crates/proxy/src/deployment.rs` | 本地 manifest、目录同步及发布编排 |
| `crates/proxy/src/response.rs` | 仅在 MCP 边界应用 compact/full；不改变内部决策依据 |
| `crates/agent/src/service.rs` | 监听、管理连接所有权、协议分发和传输 |
| `crates/agent/src/tools/` | 文件、命令、进程、系统画像、部署、更新等平台实现 |
| `crates/version`、`crates/build_version.rs` | 共享版本展示及构建信息注入 |
| `deploy/remote-ops-agent.service` | Linux 推荐监督策略 |

表中 `mcp/` 下的相对路径均以 `crates/proxy/src/` 为根。新增工具不添加旁路注册表或未校验的默认分发分支。

## 一次调用的边界

proxy 先消费 response_mode，再校验类型、未知字段、范围、格式和参数关系。预检不连接设备或执行文件/命令操作；batch 所有子调用预检后才允许执行首项。Agent 独立校验参数，并根据当前文件类型、权限和平台状态处理请求。

内部使用完整结果进行状态决策，最后才在 MCP 输出边界精简。错误来源和 outcome 分开记录：远端确定性错误可保留同步会话，发送后的传输/协议失败标记 unknown、关闭会话并保留恢复上下文。probe 可在用户请求发送前重连，不能重放已发送操作。

## 平台与事务设计

- environment 报告实际候选程序和 init 证据，不按 CPU/发行版划分“服务器”或“嵌入式”。命令显式选择解释器，参数和输出有界。
- 文件传输先写同目录临时文件，校验长度和 SHA-256 后提交；续传校验前缀。Unix 同步父目录，Windows 使用 replace/write-through。
- sync_directory 在 staging 复核 manifest 后切换，保留旧 backup；deploy_release 将链接切换、启动、健康检查与回滚放在单次 Agent 请求内。
- 自更新 helper 等待旧 Agent 退出、原子替换、稳定检查并处理回滚；有 systemd 监督时交回原单元。结果以关联 attempt_id、候选哈希和前实例的最终回执确认。Linux zombie 检测有界读取 procfs，不能只用 kill(pid,0)。

## 协议与安全范围

双方随机 nonce 与固定内置值参与会话密钥派生，帧完整性和防重放保留。固定值不是部署者的秘密，协议不加密，不能提供安全的身份隔离；网络访问控制或受保护的隧道由部署环境提供。

协议结构变化须提升版本并验证拒绝旧协议；v3/v4 不降级兼容。schema/结果展示变化与 wire 变化分别审计，不能仅因“还是 JSON”就跳过兼容性判断。
