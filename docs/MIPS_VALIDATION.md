# MIPS 真机验证与 2.0.1 修复

验证日期：2026-11-09。本文保留可复现的问题、修复依据与运行边界，不记录现场账号、设备地址或临时进程标识。

## 设备与覆盖范围

Ingenic XBurst，小端 MIPS32r2/o32/soft-float，Linux 4.4.94，BusyBox 1.22.1，内存约 58 MiB（59,124 KiB）。PID 1 为 linuxrc/BusyBox init；只有 sh/BusyBox，无 systemd/Python。根目录 squashfs，`/tmp` 为 tmpfs，安装分区为可写 jffs2。

原 2.0.0（e08b6d1）通过已注册 RemoteOps 完成 150 次调用，覆盖全部 40 工具、72 项断言：

- 文本/BOM/CRLF、分页、tail、补丁冲突、搜索、文件类型/mode/符号链接。
- BusyBox shell/cwd/env、长脚本、进程信息/kill/pkill、前后台超时与输出窗口、断线重连。
- 多 Chunk 二进制上传下载、正确前缀续传、错误前缀回退、仅变化文件同步。
- Unix 发布成功、stop 失败、健康失败回滚；独立副本更新、启动失败回滚和实例切换。

三份 300,000 字节文件在 PC 上重算 SHA-256 一致。BusyBox 缺 mkfifo/head applet，夹具按实际能力改用 mknod p、dd；不能因 BusyBox 存在就假设所有 applet 可用。

## 只读 squashfs 被误报为可写

设备 mountinfo 根条目为：

```text
14 0 31:2 / / rw,relatime - squashfs /dev/root ro
```

挂载点是 rw，但 superblock 为 ro。旧代码只检查前者，报告 `read_only=false`。2.0.1 同时检查两个选项集合，任一包含独立 ro 即为只读；`errors=remount-ro` 不误判。

回归覆盖挂载点/superblock 的四种组合。修复在临时 MIPS 副本上验证 `system_info` 根目录 read_only=true，随后自动更新/回滚后仍正确。

## 自更新被未回收 zombie 阻塞

正式安装升级时，原 Agent 退出后，交互式 BusyBox shell 尚未 wait。helper 仅用 kill(pid,0) 判断存活，对 Z 状态仍成功，于是返回 unconfirmed，未开始替换。设备业务与维护入口仍在线，旧程序和候选均完整保留。

通过已有维护入口恢复旧 Agent，先让其由能 wait/reap 的包装 shell 启动。2.0.1 修复为有界读取 `/proc/<pid>/stat`，Z/X/x 视为已退出；活进程继续阻止替换，procfs 不可读时保守退回存活探测。

验证包括真实子进程退出但不 wait 的 Linux 单测，以及：

```sh
python3 tools/verify_agent_update.py --defer-reap --agent /path/to/agent --proxy /path/to/proxy
```

脚本完整更新并取得最终回执后，确认旧 PID 为 Z 才回收；随后继续测试候选启动失败回滚。普通、自延迟回收和 systemd 更新/回滚均有执行记录。旧版首次迁移条件见[迁移说明](MIGRATING_V2.md)。

## 最终发布与安装结果

2.0.1 最终提交为 e5fb0e1，协议 v4；11 目标、21 个文件从干净提交构建并独立核验。Linux workspace 167 项测试通过，Windows 163 项；两端 Clippy、三项 musl 检查、发布脚本 12 项测试通过。

设备实际安装路径 `/system/remote-ops-agent` 已成功从 2.0.0 升级至 2.0.1，最终回执 updated，前实例/attempt/候选哈希匹配；新实例上线、安装文件哈希与发布清单一致、根只读状态正确。MIPS 最终产物 SHA-256 为 `d004ef0f3d05432773716a05ec60663806218e331723a8926ea7cb90674c3366`，build ID 为 `2bd31179c0064b6085b558671c03203f`。后续产物应查自身 manifest，不能复用这份历史哈希。

## 清理与未测范围

测试目录、8 个后台测试任务和更新 helper/候选/回滚临时文件已清理，安装目录保留最终回执。恢复连接时产生过少量 tmpfs 日志；未修改设备的启动脚本、账号、网络、防火墙或业务程序。

未执行 root 设备真实 reboot，仅测试参数拒绝。当前设备的 Linux 4.4.94 证据不能推广到 3.10.14 或其他固件。后续兼容性验证采用[构建与验证](BUILDING.md#平台实测)的独立目录和端口，正式替换设备程序需明确授权。
