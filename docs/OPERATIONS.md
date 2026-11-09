# 安装与运维

首次配置 MCP 见[README](../README.md)，工具调用顺序见[AI 使用指南](USAGE.md)。本文关注设备安装、自启、升级与连接排障。

## 选择下载文件

每台机器按自己的系统和 CPU 架构选后缀。下面的后缀同时用于 `remote-ops-proxy-*` 和 `remote-ops-agent-*`，MIPS 只提供 Agent。

| 机器 | 文件后缀 |
| --- | --- |
| Windows x64 / ARM64 | `x64-win.exe` / `arm64-win.exe` |
| macOS Intel / Apple Silicon | `x64-mac` / `arm64-mac` |
| Linux x64 / ARM64 | `x64-linux` / `arm64-linux` |
| Linux ARMv7 hard-float / soft-float | `arm32hf-linux` / `arm32-linux` |
| Linux RISC-V64GC | `riscv64gc-linux` |
| Linux LoongArch64 | `loongarch64-linux` |
| Linux 小端 MIPS32r2、o32、soft-float | `mipsel-linux`（仅 Agent） |

同为 ARM32 也需匹配浮点 ABI；MIPS 文件不适用于任意大端或其他指令集设备。`Exec format error` 常见于架构选错。Linux 可先运行 `uname -m`，不确定 ABI 时查设备固件/工具链说明，不靠文件名猜测。

Unix 上先为程序设置执行权限；两端程序都可运行 `--version`，Agent 还提供 `--self-check`。从发布目录取得 `SHA256SUMS` 时，Linux 可执行 `sha256sum -c SHA256SUMS`；Windows 用 `Get-FileHash -Algorithm SHA256` 核对对应文件。编译或哈希成功不替代具体设备的运行验证。

## 启动选项

Agent：

| 参数 | 含义与默认值 |
| --- | --- |
| `--listen IP:PORT` | 默认 `0.0.0.0:8022`；改为回环地址可限制为本机/隧道访问 |
| `--max-transfer-bytes N` | 单文件上限，默认 4,294,967,296 字节（4 GiB） |
| `--version` / `-v` | 查看版本并退出 |
| `--self-check` | 返回可机器读取的版本、协议和目标信息并退出 |

proxy 参数放在 MCP 客户端的 args 中：

| 参数 | 含义与默认值 |
| --- | --- |
| `--remote IPv4:PORT` | 固定目标，未配置时为 `192.168.43.106:8022`；推荐显式设置自己的设备 |
| `--response-mode compact\|full` | 默认 compact，单次调用可覆盖 |
| `--timeout-ms N` | 远端操作 I/O 超时，默认 310,000 ms；不是后台任务的运行期限 |
| `--max-transfer-bytes N` | PC 端单文件上限，默认 4 GiB；仍受 Agent 一侧上限约束 |
| `--version` / `-v` | 查看版本并退出 |

客户端自身还有工具调用超时。长时间上传、更新或 batch 时，应给客户端足够等待预算；调整 proxy I/O 超时不会自动修改客户端或具体命令的期限。

## Linux：systemd 自启

以下操作需要管理员权限。将[单元模板](../deploy/remote-ops-agent.service)另存为 `remote-ops-agent.service`，与下载的 Agent 放在同一目录后执行。服务使用 `/usr/bin/remote-ops-agent`；已有安装请先读取实际 ExecStart，备份程序和单元配置。

```sh
sudo install -m 755 ./remote-ops-agent-x64-linux /usr/bin/remote-ops-agent
sudo install -m 644 ./remote-ops-agent.service /etc/systemd/system/remote-ops-agent.service
sudo systemctl daemon-reload
sudo systemctl enable --now remote-ops-agent.service
systemctl status remote-ops-agent.service --no-pager
journalctl -u remote-ops-agent.service -n 50 --no-pager
```

按设备架构替换源文件名。修改监听地址或安装路径时修改单元并 daemon-reload。若已有 Agent 占用同一端口，先安排切换，避免两个实例竞争监听。

自更新依赖单元保留以下配置：

```ini
KillMode=process
Restart=on-failure
```

检查实际生效值：

```sh
systemctl show remote-ops-agent.service -p ExecStart -p MainPID -p UnitFileState -p KillMode -p Restart
```

模板以 root 运行。若改为其他账号，应确保所需文件和程序可访问；Linux reboot 需要 root，自更新还需要安装目录的写入权限和磁盘空间。

## 无 systemd 的 Linux、macOS 与 Windows

先按 README 前台运行确认可用，再接入已有 init、launchd 或任务计划。RemoteOps 不自动为这些平台安装自启项；程序运行也不代表已经配置了开机启动。

Unix 可在确认安装目录可写且工具存在后使用后台包装 shell：

```sh
nohup sh -c './remote-ops-agent --listen 0.0.0.0:8022 & child=$!; wait "$child"' </dev/null >remote-ops-agent.log 2>&1 &
```

在 Agent 所在目录运行，日志路径须可写；设备没有 nohup 时使用其已有服务管理方式。包装 shell 的 wait 能回收旧进程，尤其对首次从 2.0.0 自更新有用。它不提供崩溃自动重启或开机自启。嵌入式设备先确认只读根目录、可写分区、空间和 BusyBox 实际 applet，再决定安装位置。

Windows 可用 PowerShell 前台启动；若使用任务计划，填写可执行文件绝对路径和参数 `--listen 0.0.0.0:8022`，选有适当权限的账号，首次创建后主动运行并验证。不要把控制台程序直接注册为原生 Windows 服务程序。

## 更新 Agent

当前 2.0.1 与 2.0.0 均使用远端协议 v4。跨 v3/v4 及 2.0.0 首次升级限制先读[迁移指南](MIGRATING_V2.md)。

1. 用 `remote_probe`、`environment`、`agent_info` full 确认目标、版本、build.target、安装路径和 self_update/update_receipts 能力。
2. 选择与 Agent **build.target** 一致的候选文件，放在 proxy 电脑。检查安装分区空间和写权限，保留旧程序及现有维护入口。
3. 调用 `agent_update`，客户端工具超时需覆盖上传和等待时间。示例参数如下，文件路径属于 PC：

```text
agent_update {"local_path":"D:\\releases\\remote-ops-agent-arm64-linux","timeout_ms":120000,"poll_interval_ms":1000,"probe_timeout_ms":5000}
```

4. 检查 status 和关联本次尝试的 update_receipt。成功应为 updated，并核对新版本、build、runtime.instance_id 和实际安装文件哈希。systemd 设备还要确认原单元 active/enabled，MainPID 为当前 Agent。
5. rolled_back 表示候选失败后恢复旧程序；timed_out/unconfirmed 先读取回执、recovery 路径和日志，确认 helper 是否仍运行。不要删除未核实的恢复文件，也不要自动重试更新。

更新会短暂断开连接，后台任务不跨 Agent 重启保留。确认所有任务结束后再更新。Agent 更新不会自动替换电脑上的 proxy；替换本地 proxy 后重新加载客户端 MCP 服务。

## 连接与故障排查

先区分“端口不通”“握手失败”和“命令执行失败”。维护 SSH/Telnet、业务服务与 Agent 是不同入口，端口和防火墙规则也独立。

| 现象 | 验证步骤 |
| --- | --- |
| refused / 连接被拒绝 | 在设备确认进程仍运行，检查监听 IP 和端口；`127.0.0.1` 只接受本机连接。 |
| timeout / 连接超时 | 确认地址、路由、VPN、主机防火墙和云安全组；先查 Agent 端口，别直接重启设备。 |
| 握手时被关闭 | 比对两端 `--version` / Agent `--self-check`，确认协议配套；旧协议不会自动降级。 |
| cached 但调用失败 | 先 remote_probe；必要时对相同地址 set_remote 丢弃缓存，再主动探测。请求已经发出的命令先确认实际状态。 |
| manager_busy | 另一管理连接正在执行请求；等待其结束，不中断或排队已有操作。 |
| unsupported | 查 environment、Agent 能力和参数；Windows 的 chmod/Unix mode/发布事务、macOS 的 reboot 不支持。 |
| 命令乱码 | 原生程序可能输出系统代码页；配置程序自身的 UTF-8 输出，或选适当 shell。 |
| 更新后未恢复 | 查安装路径旁 update-result 回执、helper/manifest、候选和回滚文件；保存证据，按迁移说明处理旧版 zombie 问题。 |

systemd 设备可检查 `ss -lntp`、`systemctl status` 和 `journalctl`；BusyBox 设备按实际 applet 使用 `netstat`、`ps` 等。不要假设每个平台都有相同命令。

需要隧道时，可通过已有 SSH 通道把本机端口转发至设备 Agent，例如：

```sh
ssh -N -L 18022:127.0.0.1:8022 user@device
```

然后让 proxy 连接 `127.0.0.1:18022`。SSH 的账号认证、网络访问和端口放行需先可用。直接连接只接受 IPv4 地址和非零端口；示例中修改客户端配置不会改变设备监听地址。
