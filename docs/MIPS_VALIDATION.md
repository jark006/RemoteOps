# MIPS 真机验收（2026-11-09）

以下记录的是 2.0.0 运行期间的故障定位与独立副本修复验证。该修复正式纳入 2.0.1，协议保持 v4；2.0.1 全平台产物通过校验后，使用 agent_update 更新设备原安装路径。正式升级结果以 helper 最终回执、设备版本/实例/二进制哈希以及根文件系统 read_only=true 为验收依据。

设备为 `192.168.43.192:8022`，Ingenic XBurst、小端 MIPS、Linux 4.4.94、BusyBox 1.22.1，物理内存约 59,124 KiB。Agent 2.0.0 / protocol v4，原提交 e08b6d1b7d1b。PID 1 为 linuxrc/BusyBox init，检测到 sh 和 BusyBox，无 systemd/Python；根目录是 squashfs，`/tmp` 为 tmpfs，`/system` 为可写 jffs2。

通过本会话已注册的新版 RemoteOps 真实调用 150 次，覆盖全部 40 个工具，72 项断言通过。测试范围包括 UTF-8/BOM/CRLF 读取、按字节/行分页、tail、补丁及哈希冲突、目录分页/正则搜索、文件变更、mode/符号链接/特殊文件拒绝、BusyBox applet 实际执行、长脚本、cwd/env、退出码、超时、前后台输出上限/游标、pids/process_info/kill/pkill、跨 Chunk 二进制传输、前缀校验续传/安全回退、增量目录同步、发布成功/健康失败回滚/停止失败、连接复用/同地址重连及生命周期等待。

三个 300,000 字节下载文件（普通、正确前缀续传、错误前缀回退）在 PC 端复核 SHA-256，均为 `3c65ea93424a9c362fec0e3a69ea36031e8a358441479dd665cc6110eabe7b08`。BusyBox 实际缺少 mkfifo/head applet，测试通过已确认存在的 mknod p、dd 等调整夹具，没有把 BusyBox 文件存在当作所有 applet 可用的保证。

## 发现并修复的问题

设备 `/proc/self/mountinfo` 根条目为：

```text
14 0 31:2 / / rw,relatime - squashfs /dev/root ro
```

旧实现只检查挂载点选项，导致 `system_info.filesystems.mounts[].read_only=false`。现在同时检查挂载点和 superblock 选项，任一包含独立 `ro` 即报告只读；`errors=remount-ro` 不误判。增加挂载点/superblock 四种组合的回归测试，未修改工具参数、输出字段或 wire 协议。

Linux workspace 166 项测试通过、1 项需指定独立远端的 live 用例默认 ignored；Linux Clippy、fmt、diff 检查通过。修复版使用 Nightly/build-std 构建 MIPS32r2/o32/soft-float 静态 musl，并通过构建器的 ABI、内嵌来源与哈希校验。

测试产物位于 `target/validation/live-tools-20261109/mips-mount-fix/`，Agent SHA-256 为 `9db602fc92c2a5c27df046d02056375e96f5b38e67c6b6224420ad16347dfc77`，构建源码快照 SHA-256 为 `fbf26948c1a10dc3b3d31e4635628745f19b5bbb1fab517a6c5600b5e8e9f62e`。该来源快照包含修复代码和当时文档，不包含随后新增的本验收记录；正式 `Releases/` 未修改。

在 `/tmp/remote-ops-full-20261109-mips-2ddfbaba` 中创建独立 Agent 副本和 18404 临时端口：原版副本通过 agent_update 更新到修复版，helper 最终回执和实例关联正确，真机 `system_info` 报告根只读。随后真实重启独立副本，验证 offline_then_online；再上传通过 self-check 但退出 37 的 shell 候选，验证自动回滚到修复版、回执 rolled_back、恢复后系统信息仍正确。

## 清理与边界

所有测试目录已删除，8 个后台任务均已 wait/close，未残留匹配测试路径的进程。`/tmp` 使用量从测试中约 7,380 KiB 恢复至 428 KiB，可用内存约 35,408 KiB。原 `/system/remote-ops-agent` 保持 PID 878、实例 `2ddfbabac17b90cc463d6ef1e6ad9c71`，未替换；因此原服务仍有上述只读信息误报，修复已在临时副本验证。

未执行设备的真实 root reboot，只测试参数越界拒绝。此次真机内核为 4.4.94，不能据此承诺其他内核（例如 3.10.14）或固件上的行为。机器可读用例、调用记录及构建/回归日志位于 `target/validation/live-tools-20261109/` 的 mips 相关文件。
