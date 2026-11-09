# 构建、验证与发布

普通用户安装预编译程序见[README](../README.md)。本文面向开发和发布；实现约束从[AGENTS](../AGENTS.md)进入。

## 本地开发

需要支持当前 workspace 和 Cargo.lock 的 Rust 工具链。源码使用 edition 2024；2.0.1 Windows 发布采用 Rust 1.95，MIPS 使用 Nightly 和 rust-src。不要在开发验证时无意升级锁文件中的依赖。

```sh
cargo build --locked --workspace
cargo run -p remote-ops-agent -- --listen 127.0.0.1:18022
```

另一个终端启动 proxy 或将它交给 MCP 客户端：

```sh
cargo run -p remote-ops-proxy -- --remote 127.0.0.1:18022
```

调试程序默认在 target/debug；Windows 有 `.exe`。stdio 是协议通道，人工运行 proxy 时不出现命令提示符是正常行为。

## 验证变更

普通 Rust 实现变更运行：

```sh
cargo fmt --all -- --check
rustfmt --edition 2024 --check crates/build_version.rs
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

构建器和发布工作流变更还运行（Python 3.9+，无需第三方依赖）：

```sh
python3 -m unittest discover -s tools -p 'test_*.py'
```

涉及共享代码、Agent 或发布兼容性时，补交叉检查：

```sh
cargo check --locked --workspace --target x86_64-unknown-linux-musl
cargo check --locked -p remote-ops-agent --target aarch64-unknown-linux-musl
cargo check --locked -p remote-ops-agent --target armv7-unknown-linux-musleabihf
```

Linux 专属逻辑必须在 Linux 执行测试；Windows/macOS 分支在对应平台验证。修改结果字段要检查真实双端与 MCP stdio 的 schema，不只测内部函数。文档变更只核验相关链接、示例和契约一致性，无需为了改文字重跑全平台编译。

| 测试入口 | 覆盖 |
| --- | --- |
| `crates/protocol` 测试 | 认证、协议拒绝、HMAC/序号、帧限制和绝对截止时间 |
| `crates/agent/tests/management_takeover.rs` | 单管理者接管与 busy 行为 |
| `crates/proxy/tests/end_to_end.rs` | 二进制、续传、同步、发布和任务等真实双端流程 |
| `environment_contract.rs` | 平台发现、实际 shell、cwd/env 和可选真机往返 |
| `response_contract.rs` | compact/full、输入/输出 schema、stdio |
| `reliability_contract.rs`、`workflow_failures.rs` | 输出限制、不确定状态、连接与事务失败路径 |

表中未带目录的 Rust 测试文件位于 `crates/proxy/tests/`。更改哪个行为，就选择对应回归；验证通过后无需无理由重复运行。

## 平台实测

以下操作会启动进程、传输文件或切换测试链接。仅使用获授权的测试主机和独立目录，失败时检查残留。

```sh
# live 用例默认 ignored；明确指定 Unix 测试 Agent 和可写工作根目录。
REMOTE_OPS_TEST_ADDR=192.168.1.50:18022 REMOTE_OPS_TEST_ROOT=/tmp/remoteops-test \
  cargo test --locked -p remote-ops-proxy --test environment_contract \
  live_unix_agent_round_trip -- --ignored --nocapture

# 测试脚本与产物须在同一运行平台。
python3 tools/verify_agent_update.py --agent /path/to/agent --proxy /path/to/proxy

# Linux：故意让旧 Agent 保持 zombie，完整更新成功后才回收。
python3 tools/verify_agent_update.py --defer-reap --agent /path/to/agent --proxy /path/to/proxy

# Linux root + systemd：创建独立 transient unit，测试更新/回滚后仍受监督。
python3 tools/verify_agent_update.py --systemd --agent /path/to/agent --proxy /path/to/proxy

# Linux root：静态 Agent + 静态 BusyBox，在无 procfs/systemd 的 chroot 验证。
python3 tools/verify_embedded_environment.py \
  --agent /path/to/static-agent --proxy /path/to/proxy --busybox /path/to/static-busybox
```

不要把用户设备重启作为常规测试；未测试的成功重启路径明确记录。自更新脚本使用临时副本，不能替代获授权的实际安装更新。实测证据见[重构记录](REFACTOR.md)和[MIPS 记录](MIPS_VALIDATION.md)。

## 发布构建

唯一目标矩阵是 [targets/releases.json](../targets/releases.json)：11 个目标、21 个程序，MIPS 仅 Agent。版本取自 workspace；`buildAll.ps1` 是 Python 构建器的 PowerShell 包装，不维护另一套目标或版本。

准备 Rust、Python 3.9+、Zig 和 cargo-zigbuild；PowerShell 包装脚本需 7.2+。普通目标安装与所选工具链匹配的 rust-std，例如：

```sh
cargo install --locked cargo-zigbuild
rustup target add --toolchain 1.95 x86_64-unknown-linux-musl
rustup toolchain install nightly --component rust-src
python3 tools/build_release.py --list-targets
```

全矩阵需要为矩阵中所有非自定义目标安装 rust-std；不要对 MIPS 自定义 JSON target 执行 rustup target add。Zig 的安装方式按宿主系统选择，并记录实际 `zig version`。macOS 交叉链接可能受 SDK 和工具链版本影响，构建成功后仍应真机验证。

先提交固定源码，确认工作区干净，再构建至**全新目录**：

```sh
# 默认完整矩阵，输出 Releases/v<workspace-version>，普通目标使用当前工具链。
python3 tools/build_release.py

# 指定工具链和部分目标，每个 --target 可重复。
python3 tools/build_release.py --toolchain 1.95 \
  --target aarch64-apple-darwin --target x86_64-unknown-linux-musl \
  --output Releases/test-build

# 使用原生 Cargo linker，仍遵守相同的收集和校验规则。
python3 tools/build_release.py --native --target aarch64-apple-darwin --output Releases/native-mac
```

Windows：

```powershell
pwsh -File ./buildAll.ps1 -Toolchain 1.95
./buildAll.ps1 -Target aarch64-apple-darwin,x86_64-unknown-linux-musl -Toolchain 1.95 -Output Releases/test-build
```

要直接交付到 Releases 根目录，先将整个旧 Releases 移到 `target/validation/` 下唯一命名的备份，确认源/目标绝对路径，再用 `-Output Releases` 或 `--output Releases`。已有目录不会覆盖，也不逐项混入旧文件；构建失败检查日志后重试全新输出。

### MIPS 专用规则

`targets/mipsel-unknown-linux-musl.json` 固定 little-endian MIPS32r2、o32、soft-float、静态 musl。统一构建器始终为它选择 Nightly，即使 --toolchain 指定了普通目标工具链。

独立开发构建命令：

```sh
cargo +nightly build --locked -p remote-ops-agent --release \
  --target targets/mipsel-unknown-linux-musl.json \
  -Z json-target-spec -Z build-std=std,panic_abort
```

不得用 cargo zigbuild 将该 Rust 三元组直接映射到 Zig。JSON target 直接调用 Zig 的 `mipsel-linux-musleabi` ABI 名称。Tier 3 没有预装目标标准库，需 rust-src/build-std；不存在预编译 rustlib 的链接器提示须结合最终成功状态、静态 ELF 和 ABI 校验判断。真机 Linux 4.4.94 的结果不能推断其他固件内核兼容。

## 产物与来源核验

构建只使用本次 Cargo JSON compiler-artifact 报告的 executable；禁止固定 target 路径回退收集。构建期间源码变化、编译失败、缺失产物或校验失败均不产生完成目录。

每个产物检查 PE/Mach-O/ELF 架构、静态 musl、ARM 浮点 ABI、MIPS32r2/o32/soft-float，并匹配内嵌 version/target/profile/source_sha256/build_id。sibling staging 使用被 Git 忽略的 `.remote-ops-release-*`；临时输出不能使后续目标被标记 dirty。

最终目录含：

- 可执行文件：由矩阵列出的目标与程序集合。
- `manifest.json`：版本、协议、是否完整矩阵、工具链、源码逐文件哈希、唯一 build ID 和各产物大小/哈希/metadata。
- `SHA256SUMS`：覆盖产物和 manifest。

两端 `--version` 第二行携带来源标记，可不执行异架构程序就核验。`REMOTE_OPS_SOURCE_SHA256` / `REMOTE_OPS_RELEASE_BUILD_ID` 由构建器注入；普通开发构建允许 unknown/development。`REMOTE_OPS_GIT_REVISION` 可显式指定，否则读取 Git；不将 development/旧 build ID 冒充本次发布。

发布使用 --locked/release；workspace 开启 LTO、单 codegen unit、体积优化和符号剥离，SHA-256/正则热路径单独优化。参数取舍集中在 Cargo.toml，不在发布脚本重复设置。文档本身也进入来源快照：发布后更改文档不会追溯改变既有 manifest，重新发布时需完整重建。
