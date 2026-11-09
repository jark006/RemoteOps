# RemoteOps

让 AI Agent 在你的授权下操作远端 Linux、Windows、macOS 和嵌入式设备：查看文件、运行命令、传输文件、管理进程、同步目录和发布程序。

你的电脑运行 `remote-ops-proxy`，AI 客户端通过 MCP 调用它；被控设备运行 `remote-ops-agent`。两端可以使用不同的操作系统。

Agent 具有启动它的账号权限。请通过可信内网、VPN 或受保护的隧道连接；连接本身不加密，也不提供独立的安全凭据。需要命令执行、重启或系统目录写入时，请先明确授权范围。

## 1. 下载两端程序

从 [Releases](https://github.com/jark006/RemoteOps/releases) 下载配套版本：

| 放在哪里 | 下载什么 | 按什么选择后缀 |
| --- | --- | --- |
| 运行 AI 客户端的电脑 | `remote-ops-proxy-*` | 这台电脑的系统和 CPU 架构 |
| 被控设备 | `remote-ops-agent-*` | 设备的系统和 CPU 架构 |

例如，Windows x64 电脑控制 ARM64 Linux 设备，需要 `remote-ops-proxy-x64-win.exe` 和 `remote-ops-agent-arm64-linux`。完整的[文件名对照表](docs/OPERATIONS.md#选择下载文件)包含 ARMv7、MIPS 等设备。

下文将文件重命名为 `remote-ops-proxy`、`remote-ops-agent`；Windows 保留 `.exe`。也可以在配置中使用下载后的完整名称。

## 2. 在被控设备启动 Agent

Linux / macOS，在 Agent 所在目录执行：

```sh
chmod +x ./remote-ops-agent
./remote-ops-agent --listen 0.0.0.0:8022
```

Windows，在 PowerShell 中执行：

```powershell
.\remote-ops-agent.exe --listen 0.0.0.0:8022
```

首次连接时保持终端打开，确认出现监听地址。允许你的电脑访问设备 TCP 8022 端口；云端设备还需检查云防火墙和安全组。只供本机使用时，可监听 `127.0.0.1:8022`。

验证连接后，按[运维指南](docs/OPERATIONS.md)配置后台运行或 Linux 开机自启。设备只需允许所需操作的账号权限；Linux 系统重启需要 root。

## 3. 在 AI 客户端配置 MCP

将 proxy 放到电脑的固定目录，在客户端的 MCP 设置中添加一个 **stdio** 服务。`command` 指向电脑上的 proxy，`args` 中填写设备 IPv4 和端口。推荐使用绝对路径。

Codex：编辑 `~/.codex/config.toml`，加入：

```toml
[mcp_servers.remote-ops]
command = "C:/Tools/remote-ops-proxy.exe"
args = ["--remote", "192.168.1.50:8022"]
tool_timeout_sec = 900
```

Linux / macOS 的 `command` 可以改成 `/home/yourname/tools/remote-ops-proxy`。示例将客户端工具等待时间设为 900 秒，便于传输和更新；按实际任务调整。配置说明见 [Codex 官方 MCP 文档](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)。

Claude Code 等接受 `mcpServers` 配置的客户端可使用：

```json
{
  "mcpServers": {
    "remote-ops": {
      "type": "stdio",
      "command": "C:/Tools/remote-ops-proxy.exe",
      "args": ["--remote", "192.168.1.50:8022"]
    }
  }
}
```

配置文件位置和加载方式由客户端决定。保存后重新加载 MCP 服务或重启客户端。客户端会启动 proxy；不必先在终端手动运行它。不要把设备上的 Agent 路径填入 `command`。

## 4. 开始使用

可以直接向 AI 说明目标和范围：

> 用 RemoteOps 连接 192.168.1.50:8022，先确认平台、可用 shell 和设备状态，只做读取检查。

> 查看 /var/log/myapp.log 的最后 100 行，定位错误，先说明原因再修改。

> 把电脑上的 D:\deploy\app.bin 上传到设备 /tmp/app.bin，校验文件哈希，不要启动程序。

> 检查后台任务运行结果，增量读取输出，完成后释放任务。

连接通常先调用 `remote_probe`，再调用 `environment`。反馈默认简洁；需要网络、磁盘或构建信息时，让 AI 使用 `response_mode: "full"`。

切换设备可调用 `set_remote`。该设置只对当前 proxy 进程有效；长期固定设备应修改 MCP 启动参数。上传、下载、目录同步和更新中的 `local_path` 始终属于运行 proxy 的电脑。

## 常见问题

| 现象 | 先检查什么 |
| --- | --- |
| 客户端找不到工具 | proxy 路径、执行权限、MCP 服务是否重新加载 |
| 连接被拒绝 | Agent 是否启动、实际监听地址和端口 |
| 连接超时 | IP、路由、主机防火墙、云安全组；SSH 正常不代表 8022 已放行 |
| 能连端口但握手失败 | 两端版本是否配套；旧版升级见[迁移指南](docs/MIGRATING_V2.md) |
| shell 或工具不可用 | 让 AI 先读取 `environment`，选择设备实际支持的解释器 |
| 命令超时或中途断线 | 先确认远端实际状态，再决定下一步；不要直接重跑有副作用的命令 |

更多步骤见[连接与故障排查](docs/OPERATIONS.md#连接与故障排查)。

## 继续阅读

- [AI 使用指南](docs/USAGE.md)：推荐调用顺序、工具选择、分页、长任务和错误恢复。
- [工具参考](docs/TOOLS.md)：全部工具、参数与结果说明。
- [运维指南](docs/OPERATIONS.md)：下载文件选择、自启、升级、备份和排障。
- [迁移指南](docs/MIGRATING_V2.md)：从旧版本迁移。
- [文档导航](docs/README.md)：开发、构建和验证资料入口。
