# 在 RemoteOps 仓库工作

优先使用简体中文。本文面向修改仓库的开发 Agent；使用 RemoteOps 管理设备请读 [AI 使用指南](docs/USAGE.md)。

## 开始前

- 有 `.codegraph/` 时，理解或定位代码先调用 `codegraph_explore`（或 `codegraph explore`），再做未被覆盖的文本搜索。不要自行创建索引。
- 检查工作区现有改动，保留用户工作。先读[架构说明](docs/ARCHITECTURE.md)，再按下表加载相关契约，避免每次读取全部历史记录。
- 代码和已注册工具的 schema 是当前实现证据。文档与实现不符时明确定位差异，不根据平台名称猜测能力。

| 修改范围 | 必须阅读 |
| --- | --- |
| 连接、认证、帧、stdio | [实现契约：架构边界](docs/CONTRACTS.md#架构边界) |
| 工具名称、schema、参数、响应、batch | [工具契约](docs/CONTRACTS.md#工具契约)、[工具参考](docs/TOOLS.md) |
| 文件传输、同步、发布、进程或生命周期 | [对应工具契约](docs/CONTRACTS.md#工具契约)、[平台行为](docs/CONTRACTS.md#平台行为) |
| 构建、版本、发布产物 | [构建与验证](docs/BUILDING.md) |
| 文档 | [文档分工](docs/README.md#维护文档)，只更新与变更有关的用户说明和参考资料 |

## 不可破坏的边界

- MCP JSON-RPC 只在 proxy stdio；stdout 不写日志，文件字节不编码进 MCP JSON。
- 保持同步实现、单活动管理者和串行请求，不引入 async runtime 或请求多路复用。
- proxy 与 Agent 双重校验；所有调用先预检，batch 全部预检后才执行首项。
- 请求发出后的断线属于状态不确定，丢弃连接并报告恢复上下文，禁止自动重放。
- 所有输入、采集、编码、输出及等待有界；认证连接空闲时不主动关闭。
- 文件操作保持精确路径与类型限制；传输校验长度和 SHA-256，完整校验后才提交目标。
- 控制帧、wire 类型或帧格式变化必须提升远端协议并补拒绝测试；当前协议为 v4。
- 固定使用 `BUILTIN_PSK = b"JARK006_PSK"`，不要求配置 `REMOTE_OPS_PSK`；不将其宣传为安全认证或加密。

## 实现与验证

- 工具唯一注册表是 `crates/proxy/src/mcp/catalog.rs`。新增工具必须补齐 schema、校验、执行穷尽匹配和合法/非法参数契约测试，不能恢复默认转发分支。
- 名称、参数、默认值、限额或结果字段改变时，同步更新实现契约、工具参考和测试；用户操作流程改变再更新 README/使用指南。不要把实现细节重新堆进 README。
- Unix 原子替换尽量保留 mode 并同步父目录；Windows 覆盖用 replace/write-through 语义，不先删除目标。
- 按[构建与验证](docs/BUILDING.md#验证变更)完成适合改动的检查。Linux 专属逻辑在 Linux 测试，Windows 检查通过不能替代它。
- 真机验证使用独立目录、端口、进程或服务；记录清理与未测边界。安装、替换常驻 Agent、重启或修改服务配置须在用户授权范围内。
- 未确认事务保留 recovery 路径，不自动删 staging/备份。不要把编译、短暂上线或相同 revision 当作完整更新成功。

## 交付

- 当前版本来自 workspace `Cargo.toml`，同时更新锁文件；不在多个脚本维护第二个版本号。
- 只有被请求时才提交、推送、发布或更新设备。发布先固定源码，再用统一构建器收集本次 Cargo 报告的 executable；构建期间不要改源码或文档。
- 保留旧发布目录，向全新输出目录构建；核验 manifest、SHA256SUMS、架构/ABI 和内嵌来源。细节见[发布构建](docs/BUILDING.md#发布构建)。
- 最终说明变更、验证结果和实测范围。历史报告供证据查询，不作为每次任务的必读上下文。
