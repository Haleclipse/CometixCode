# Windows x86_64 可用性审计

- 审计日期：2026-11-02
- 范围：只审计，不改实现代码；目标是可发布的 Windows x86_64 二进制。
- 当前目标：`x86_64-pc-windows-gnu`。
- 状态分类：
  - **正常**：已有 Windows 分支，当前代码路径没有发现平台性阻断。
  - **能编译但行为不对**：Windows 分支存在，但语义、默认行为或安全属性与 Unix 不同。
  - **不可用**：当前路径不支持 Windows，或功能尚未实现。
- 工作量：S（半天以内）、M（约 1–3 天）、L（约 3–7 天）、XL（超过一周，涉及架构/发布设施）。估计只覆盖实现与验证，不含签名证书、商店审核等外部等待。

## 结论摘要

当前代码可以在 Windows runner 上通过 GNU target 的 `cargo check --lib --tests`，但这不等于 Windows 版可用：

1. **核心 Bash 执行依赖 Git Bash**，不是原生 PowerShell；没有 Git for Windows 时主 shell 不可用。
2. **sandbox 在 Windows 明确关闭**：seccomp/bwrap/Seatbelt 都没有 Windows 实现。若把 sandbox 当作安全保证，当前 Windows 版不能宣称安全隔离。
3. **搜索、凭据、内存指标、更新器和安装器存在行为缺口**：其中 Global Search 直接执行 PATH 中的 `rg`，不会使用发布包旁的 vendored `rg.exe`；凭据在 Windows 退回明文文件；RSS 指标在 Windows 恒为 0；自动更新曾为 no-op，现已由 GitHub Releases 实现（见 §7 的 2026-11-09 更新）。
4. **PTY/终端交互没有 Windows ConPTY 路径**。当前 shell 使用 stdin/stdout/stderr pipe，适合非交互命令，不适合需要真实终端的程序。
5. **当前 `windows-check` 只验证编译**，不运行 Windows 测试、不构建 release binary、不运行 `.exe` 冒烟、不验证归档布局和外部二进制。

## 逐项审计

### 1. seccomp / sandbox

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/sandbox/sandbox_adapter.rs:306-309`、`:335-347` | **不可用**。`is_supported_platform()` 只返回 macOS/Linux；Windows 即使设置 `sandbox.enabled=true` 也会被关闭。 | 第一阶段明确把 Windows 标成“sandbox unavailable”，启动/doctor/UI 给出可见提示；不要静默把未隔离执行伪装成 sandbox。 | S |
| `src/utils/sandbox/sandbox_adapter.rs:34-59` | **不可用**。seccomp helper 在非 Linux 上直接报错；`src/utils/sandbox/sandbox_adapter.rs:62-132` 的 BPF/prctl 也只编译 Linux。 | Windows 不做 seccomp 降级替代；若需要安全边界，改用 Windows Job Object + restricted token/AppContainer，并单独定义权限模型。 | XL |
| `src/utils/sandbox/sandbox_adapter.rs:397-425`、`:923-1063` | **不可用**。实际 wrapper 只有 Seatbelt 和 bubblewrap；其它平台返回“不支持”。 | 短期禁用 sandbox 并提示；长期实现 Windows backend，且必须有真实拒绝写入、拒绝网络、进程树测试。 | XL |
| `src/utils/sandbox/network_proxy.rs:1178-1237` | **正常（非 sandbox 代理）/不可用（Linux bridge）**。Windows 可启动进程内 localhost HTTP/SOCKS proxy，但 Linux 专用的 socat Unix-socket bridge 在 Windows 是 no-op；因此不能把它当成 sandbox 网络隔离。 | 保留普通代理能力；Windows sandbox backend 完成前，不要把代理本身标作隔离。 | M（代理）/XL（隔离） |
| `src/utils/sandbox/sandbox_adapter.rs:277-304` | **正常但不适用于 sandbox**。依赖检查只检查 Linux 的 `bwrap`/`socat`、macOS 的 `sandbox-exec`，Windows 不会报告 sandbox 缺失，因为平台本身被判不支持。 | doctor 中单独报告 `Windows sandbox: unavailable`，不要显示成“依赖齐全”。 | S |

### 2. Bash 默认值、shell 选择和 PowerShell

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/shell/mod.rs:93-96`、`src/utils/windows_paths.rs:62-120` | **能编译但有部署前置条件**。Windows 默认直接寻找 Git Bash；优先固定安装路径，再调用 `where.exe git` 推导 `bash.exe`。Git Bash 不存在时主 shell 不可用。 | 发布说明和启动诊断明确要求 Git for Windows；缺失时给出安装/配置提示。 | S |
| `src/utils/shell/mod.rs:98-136` | **能编译但行为受限**。`CLAUDE_CODE_SHELL` 的 bash/zsh override 在 Windows 早于该分支返回，因此 Windows 不走 Unix shell 选择逻辑。 | 保持“Git Bash 是 Bash provider”的契约，或明确支持 Windows 下的 `NYAN_GIT_BASH_PATH`；不要把任意 PowerShell 路径当作 Bash。 | S |
| `src/utils/shell/bash_provider.rs:101-164`、`src/utils/bash/shell_snapshot.rs:100-217` | **正常（前提是 Git Bash）**。snapshot、`source`、`shopt`、`pwd -P` 和 POSIX 路径转换都是 Bash 语义；原生 `cmd.exe`/PowerShell 不能执行这些脚本。 | 把 Git Bash 写入 Windows 发行版运行时要求；不要求原生 PowerShell 兼容 Bash snapshot。 | S |
| `src/utils/shell/powershell_detection.rs:28-43`、`:66-89` | **正常**。按 `pwsh.exe` → `powershell.exe` 探测，Windows 不检查 Unix execute bit。 | 保持；补 Windows runner 的真实探测/执行测试，而不是只做编译。 | S |
| `src/tools/powershell_tool/mod.rs:292-345` | **正常（可选能力）**。PowerShell tool 会使用探测到的 `pwsh`/`powershell`，不存在时返回缺失提示。 | 在 Windows 冒烟中分别验证 PowerShell 5.1 和 PowerShell 7；没有时只禁用 PowerShell tool。 | M |
| `src/utils/shell/shell_provider.rs:7-21`、`src/utils/shell/mod.rs:201-203` | **能编译但语义需明确**。类型列表包含 PowerShell，但通用 `exec` 只接受 Bash；PowerShell 由专用 tool 调用，默认 hook shell 仍是 Bash。 | 文档中明确：通用 Bash 执行需要 Git Bash，PowerShell 是独立可选 tool；若目标是“无 Git Bash 的原生 Windows”，需另做通用 PowerShell provider。 | M–L |

### 3. 路径、权限和文件系统

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/windows_paths.rs:9-55` | **正常**。覆盖盘符、UNC、`/cygdrive` 和 Git Bash POSIX/Windows 转换；`src/utils/shell/mod.rs:352-370` 会把 `pwd -P` 结果转回 Windows 路径。 | 增加真实 Git Bash 工作目录、盘符、UNC 和 worktree 冒烟；当前单测主要是纯字符串。 | M |
| `src/utils/fs_operations/native.rs:60-78`、`:166-223` | **正常**。Windows 使用专门的 stat/lstat、symlink/junction 实现。 | 保持；在 Windows runner 运行 symlink/junction 权限允许时的测试。 | M |
| `src/utils/fs_operations/native.rs:103-135`、`:249-268` | **能编译但行为不对**。Unix mode 参数在 Windows 被忽略；`mkdir`/`append` 不会实现 `0o600`/`0o700` 的 Unix 语义。 | 对敏感文件改用 Windows ACL/DACL；普通文件则在文档中说明 Windows ACL 继承语义。不要仅把 mode 静默当成安全保证。 | M–L |
| `src/utils/secure_storage/plain_text_storage.rs:49-172`、`:265-300` | **正常（首发约束）**。Windows 路径走 plaintext backend，写入后设置受保护 DACL，仅当前用户可读写；凭据仍是明文文件。 | 后续实现 Windows Credential Manager 或 DPAPI backend；首发保留明文 warning，并在 ACL 设置失败时提示使用 env。 | M–L |
| `src/utils/sandbox/sandbox_adapter.rs:515-567`、`:594-628` | **不可用（随 sandbox）**。路径规范化本身可编译，但最终路径限制交给 Seatbelt/bwrap，Windows 没有对应 enforcement。 | 与 Windows sandbox backend 一起实现并测试，不能单独把路径转换当作权限隔离。 | XL |

### 4. 信号、进程树和后台任务

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/shell_command.rs:609-632` | **能编译且基本正常**。Windows 使用不带 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 的 Job Object 管理进程树，仅在超时、取消、abort 或 watchdog 路径显式终止；创建/分配或终止失败回退 `taskkill /PID /T /F`；Unix 使用负进程组 signal。 | 保持 fallback，并持续覆盖正常退出、超时、取消、后台任务和孙进程。 | L |
| `src/utils/bash/shell_snapshot.rs:220-245` | **能编译且基本正常**。snapshot 超时在 Windows 复用 Job Object，失败时回退 `taskkill`。 | 保持与 shell command 一致的进程树清理语义。 | M |
| `src/utils/shell_command.rs:495-510`、`:554-558` | **能编译但状态语义不完全相同**。Windows 没有 Unix signal 退出码；代码用进程 exit code 或合成 137/143 表示 killed/timeout。 | 对 UI/模型结果使用显式 `timed_out`/`interrupted` 字段，不要依赖 Unix 退出码；保留兼容码只用于展示。 | M |
| `src/utils/native_installer/pid_lock.rs:51-71`、`:87-95` | **能编译但行为不对**。非 Unix `is_process_running_platform` 恒为 `false`，Windows 上所有 PID lock 都会被诊断为非运行。 | 用 `OpenProcess` + `GetExitCodeProcess`（并验证 executable path，避免 PID reuse）；再实现 acquire/release/清理。 | L |
| `src/utils/native_installer/pid_lock.rs:125-133` | **不可用**。stale lock cleanup 当前是安全禁用的 no-op，与平台无关，但 Windows native installer 还没有可用的安装锁生命周期。 | 在安装器实现前明确禁用自动安装；完成 Windows installer 后再接入 lock mutation。 | L |

### 5. PTY、终端尺寸和录制

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/shell/mod.rs:288-309`、`:426-432` | **能编译但行为不对（需要真实终端的命令）**。Bash 执行使用 stdin null 或普通 pipe，不创建 PTY；PowerShell streaming 也使用普通 pipe。交互式程序可能阻塞、检测不到终端或无法正确渲染。 | 最小发布版把 shell 定位为 non-interactive，检测到交互提示时给出明确建议；若要完整支持，增加 Windows ConPTY backend。 | M（明确降级）/XL（ConPTY） |
| `src/utils/asciicast.rs:141-161` | **正常**。Windows 走 crossterm `terminal::size()`，失败回退 `(80,24)`。 | 在 Windows console、非 TTY 和 CI 三种环境验证尺寸回退。 | S |
| `src/utils/asciicast.rs:216-235` | **能编译但行为不完整**。resize 监听只编译 Unix `SIGWINCH`；Windows 录制不会收到同等的 console resize 事件。 | 用 Windows console resize/ConPTY 事件补录制 resize；在未实现前把 Windows 录制标成“不保证 resize 事件”。 | M |
| `src/utils/asciicast.rs:185-191` | **能编译但权限语义不同**。Windows 不设置 `0o600`，录制文件使用默认 ACL。 | 录制文件按 Windows ACL 处理，或在 Windows 文档中说明其访问范围。 | M |

### 6. Keychain / 凭据存储

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/secure_storage/mod.rs:30-43` | **能编译但安全属性受限**。只有 macOS 使用 keychain fallback；Windows 与 Linux 都直接选择 `PlainTextStorage`，Windows 文件 ACL 已限制为当前用户。 | Windows 后续使用 Credential Manager（推荐）或 DPAPI 加密 blob；在此之前保留带 warning 的 plaintext fallback。 | M–L |
| `src/utils/secure_storage/plain_text_storage.rs:8-12`、`:78-103` | **正常（功能）/能编译但安全属性不对**。凭据位置是 config home 下 `.credentials.json`，并支持旧路径导入；Windows 可读写，但不是系统凭据库。 | 发布前至少确认 config 目录 ACL；实现安全 backend 后验证迁移、删除和旧凭据导入。 | M |
| `src/utils/secure_storage/mac_os_keychain_storage.rs:29-99`、`:130-225` | **正常（Windows 不会误调用）**。`security`/macOS keychain 代码在非 macOS 编译为 no-op/缓存分支。 | 不需要 Windows 替代 `security`；替代点是 `get_secure_storage()` 的 Windows backend。 | M–L |

### 7. 自动更新、安装器和发布资产

> 2026-11-09 更新（取代下方前三行的当时结论）：自动更新已改为读取 `blxzer77/nyan`
> GitHub Releases。Windows 发布 zip 带 `SHA256SUMS` 校验，`nyan update` 用
> rename-swap 原子替换正在运行的 `nyan.exe`，后台检查只提示不安装。npm、
> package-manager 与 local-installer 更新路径已从代码中删除。

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/auto_updater.rs`（原 `:86-91`、`:146-190`、`:193-210`） | **已实现（2026-11-09）**。GitHub Releases 查询、`nyan-vX.Y.Z` 解析、平台资产选择、`SHA256SUMS` 校验、tar.gz/zip 解包、原子替换与 Windows rename-swap 均在 `src/utils/auto_updater.rs`；后台只通知，`nyan update` 才安装。 | 保持 SHA256 校验；签名/SmartScreen 见 P1。 | 已完成 |
| `src/utils/native_installer/package_managers.rs`（已删除） | **已删除**。Nyan 没有 package-manager 发行渠道，原检测 seam 只服务已移除的更新 UI。 | 未来若发布 winget/MSIX，再单独引入对应安装器。 | —— |
| `src/utils/local_installer.rs`（已删除） | **已删除**。只探测 Claude Code 的 npm local install，与 Nyan 的发布方式无关。 | Windows 二进制不依赖 Node/npm。 | —— |
| `.github/workflows/release.yml`（发布）、`.github/workflows/ci.yml:233-251`（windows-check） | **部分可用（发布）**。`release.yml` 已存在：tag 触发构建 `x86_64-pc-windows-msvc` zip，冒烟运行 `nyan.exe --version` 和内置 `rg.exe --version`；zip 内带 `vendor/ripgrep/x64-win32/rg.exe`（ripgrep 15.2.0，SHA256 固定）。`ci.yml` 的 windows-check 仍只做 GNU target 编译。剩余缺口：代码签名与 SmartScreen；Grep、Global Search、Git Bash 的运行时 smoke。 | 签名/SmartScreen 作为发布前置或明确限制；补 Grep、Global Search、Git Bash 的 Windows 运行时 smoke。 | 待重估 |

### 8. ripgrep 和其他外部二进制

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/utils/ripgrep.rs:78-120`、`:145-169`、`:929-965` | **能编译但发布行为未闭环**。已支持 `x64-win32/rg.exe`、Windows PATH/PATHEXT 和 vendor → source tree → system fallback；但当前仓库没有 `vendor/` 资产目录，发布包若不补入 `rg.exe` 就会依赖用户 PATH。 | 发布流程必须下载并带上 Windows `rg.exe`，并在 release 冒烟运行 `rg --version`；缺失时 doctor 给出清晰诊断。 | M |
| `src/components/global_search_dialog.rs:22-44` | **能编译但行为不对**。Global Search 直接 `Command::new("rg")`，绕过 `utils::ripgrep::ripgrep_command()`；发布包即使带了 vendor `rg.exe`，用户 PATH 没有 rg 时该功能仍失败。 | 改为复用统一 ripgrep resolver，或由发布启动器把 vendor 目录加入 PATH；前者更可靠。 | S–M |
| `src/utils/which.rs:75-80` | **不可用（依赖该 helper 的 Windows 路径）**。Windows/其它非 Unix 平台的 `bun_which` 恒返回 `None`，不会按 `PATHEXT` 查找 `.exe`、`.cmd`、`.bat` 等外部命令。 | 实现 Windows `PATH` + `PATHEXT` 查找，并补 `git`、`node`、`rg.exe`、`.cmd` shim 测试；不要让调用方静默得到“命令不存在”。 | M |
| `src/utils/sandbox/network_proxy.rs:1189-1203` | **不可用（Windows sandbox 依赖）**。Linux sandbox bridge 直接启动 `socat`；Windows 不应安装/依赖它，因为 Windows sandbox backend 尚不存在。 | Windows 禁用 sandbox bridge 检查；未来 Windows backend 使用 native socket/pipe 或 Windows 兼容 bridge。 | M–XL |
| `src/utils/windows_paths.rs:94-117` | **正常**。`where.exe` 是 Windows 系统工具，用于发现 Git；调用继承受控环境。 | 保持；在没有 Git 的干净 Windows runner 验证错误信息。 | S |
| `src/hooks/use_memory_usage.rs:74-94` | **正常（不会误启动 `ps`）**。`ps` 只在 macOS 分支；Windows 不会调用 Unix 外部命令，但当前实现没有 Windows RSS provider，见下一节。 | 用 Windows API 获取 RSS，避免以 0 代表正常。 | M |
| `src/services/lsp/*` 中测试用 `Command::new("node")`（例如 `src/services/lsp/server_manager.rs:623-624`） | **可编译但覆盖不足**。这些是测试/假 LSP server 的 Node 依赖，不是主程序 Windows runtime 的保证。 | Windows CI 若要跑 LSP 行为测试，显式安装 Node 24；发布版不要把 Node 作为核心依赖，外部 LSP 由用户配置。 | M |

### 9. cfg(unix) 分支和只在 Unix 上运行的测试

#### 9.1 运行时分支

| 位置 | 现状 | 建议方案 | 工作量 |
|---|---|---|---|
| `src/hooks/use_memory_usage.rs:60-94` | **能编译但行为不对**。RSS 只实现 Linux `/proc` 和 macOS Mach/`ps`；Windows 落到 `0`，内存状态永远是 Normal。 | 用 `GetProcessMemoryInfo` 实现 Windows RSS；补阈值和失败状态测试。 | M |
| `src/utils/shell/mod.rs:35-73`、`:329-337` | **基本正常**。Windows 用文件存在性判断 shell，创建进程使用 `CREATE_NO_WINDOW`；Unix execute bit/process group 分支不会编译。 | 结合 Job Object 和 Git Bash 实测；不把 `CREATE_NO_WINDOW` 当作进程树隔离。 | M |
| `src/utils/fs_operations/native.rs:110-119`、`:171-223` | **大部分正常，权限语义不同**。Windows 有实现，但 mode 参数不能映射到 Unix 权限。 | 见“路径、权限和文件系统”。 | M |

#### 9.2 测试覆盖缺口

| 位置 | 当前覆盖 | Windows 风险/建议 | 工作量 |
|---|---|---|---|
| `src/utils/sandbox/sandbox_adapter.rs:2019-2182` | Seatbelt 测试只在 macOS；symlink escape 测试在 `cfg(unix)`。 | Windows 没有 sandbox 行为测试；实现 Windows backend 后必须增加 Windows-only enforcement tests，否则保持显式 disabled。 | L–XL |
| `src/utils/sandbox/network_proxy.rs:1376-1413` | accepted socket blocking 测试只在 Unix；Linux bridge 没有 Windows 等价物。 | Windows 只需测试普通 in-process proxy；不要伪造 Linux bridge 覆盖。 | M |
| `src/utils/shell/mod.rs:578-590`、`:634-704` | symlink/真实 Bash stdin 测试含 Unix 条件，且测试命令是 `/bin/bash`/`/bin/zsh`。 | Windows `cargo check --tests` 只证明能编译；真正运行会缺少这些路径。增加 Git Bash 路径参数化测试，或在 Windows 跳过并补等价测试。 | M |
| `src/utils/shell_command.rs:623-640`、`:686-704`、`:733-743` | 测试代码直接 `Command::new("bash")`，仅 process-group 代码用 `cfg(unix)`。 | 在 Windows 运行测试会依赖 PATH 中的 bash；CI 当前不运行测试。改为使用测试 shell fixture/Windows Git Bash，或按平台提供脚本。 | M |
| `src/tasks/local_shell_task/mod.rs:646-655`、`:714-723` | 测试直接启动 `bash`，Unix process-group 只包住部分逻辑。 | Windows 行为测试同样被 Git Bash 前置条件限制；至少加入 Windows runner 的定向执行。 | M |
| `src/utils/ripgrep.rs:997-1017`、`:1120-1236` | 多个测试用 Unix execute bits、`/bin/sh`、`/usr/bin/yes`，有 `cfg(unix)`。 | vendor `rg.exe` 和 Windows process kill 没有对应行为覆盖；增加 `rg.exe --version`、超时、kill、路径分隔符测试。 | M |
| `src/utils/fs_operations/tests.rs:18-19`、`:68-69`、`:111-112`、`:189-210`、`:275-276`；`src/utils/read_edit_context.rs:318-334` | 权限、symlink、open-for-scan 等关键测试是 `cfg(unix)`。 | Windows ACL、junction、文件锁和共享删除语义没有被这些测试覆盖；新增 Windows-specific tests，不要把 Unix 测试简单取消条件后硬跑。 | L |
| `src/constants/prompts.rs:1446-1447`、`src/memdir/team_mem_paths.rs:328-329`、`src/skills/load_skills_dir.rs:1957-1995` | 另有 Unix-only 的 uname、权限/路径、文件权限测试。 | 这些测试跳过不代表 Windows 语义正确；按功能补 Windows API/路径等价测试。 | M |

## 当前 windows-check 覆盖范围

### 实际 workflow

`.github/workflows/ci.yml:233-299` 的 job 在 `windows-latest` 上执行：

```yaml
cargo build --release --locked --bin nyan
nyan.exe --version
nyan.exe --help
nyan.exe doctor
cargo test --locked --lib windows_smoke -- --nocapture
```

Job 会下载固定版本的 MSVC `ripgrep`，校验 SHA256，运行 `vendor/ripgrep/x64-win32/rg.exe --version`，并把同一二进制放入 release 目录后再次验证。`windows_required` 由 `scripts/ci/impact.mjs:116-148` 根据改动路径和新增/删除的 Windows/cfg 模式条件触发；docs-only PR 会跳过该 job。

这组定向 smoke 覆盖 Git Bash 基本命令、PowerShell tool 基本命令、Global Search 通过统一 resolver 执行 vendored ripgrep、Job Object 超时/取消后的进程树清理（失败时回退 `taskkill /T /F`）、后台任务、盘符/UNC 路径转换和 Windows 凭据 owner-only ACL。它仍不覆盖 Credential Manager/DPAPI、更新器/安装器、zip 归档或代码签名。

## 让 Windows 版“能用”的最小清单

### P0：发布前必须完成

- [x] 发布包提供并验证 `vendor/ripgrep/x64-win32/rg.exe`；修复 Global Search 的直接 `rg` 调用，使其复用统一 resolver。（#26、#28）
- [x] 发布说明和启动诊断明确要求 Git for Windows/Git Bash；验证 `NYAN_GIT_BASH_PATH`、盘符、UNC、worktree 和基本 Bash 命令。（#28、#29、#37）
  - 已完成：README 要求 Git for Windows；启动诊断与 doctor 运行时校验 `NYAN_GIT_BASH_PATH`（存在性、可执行、是 bash，支持盘符与 UNC 路径，缺失或错误时给出清晰提示）；Windows runner 上的基本 Bash 命令 smoke；Git worktree 下基本 Bash 命令执行与路径转换 smoke；`NYAN_GIT_BASH_PATH` 运行时覆盖与错误提示验证。
- [x] Windows `--version`、`--help`、一次无模型启动冒烟通过；release 构建实际产出并可执行，而不只是 `cargo check`。（#28）
- [x] Windows sandbox 默认明确显示 unavailable；不能把普通 Bash 执行宣称为 sandboxed。涉及不可信代码时给出降级提示。（#27）
- [x] 凭据方案至少做出明确选择：首发若暂时使用 plaintext，必须限制 config 目录 ACL、显示 warning，并在发布说明中明确风险；更好的最小方案是 Credential Manager/DPAPI。
  - 决定（2026-11-03，Yuki 已定）：首发用明文文件 + owner-only ACL + 警告；Credential Manager/DPAPI 后续再做。
  - 已完成：Windows `PlainTextStorage` 写入成功后设置受保护 DACL，仅授予当前用户读写权限；不继承父目录 ACL，因此 Everyone/Users 无访问 ACE。写入成功仍返回 `Warning: Storing credentials in plaintext.`，ACL 设置失败则拒绝保存并提示使用 `CLAUDE_CODE_OAUTH_TOKEN`。
  - 后续：Credential Manager/DPAPI；继续保留明文存储风险说明。
- [x] 自动更新/安装器 UI 不得显示“可更新但实际 no-op”；首发改为手动下载提示，或完成一个可验证的 Windows installer/update path。（#27）
  - 2026-11-09 更新：已完成 GitHub Releases 更新路径；后台只提示，`nyan update` 校验 SHA256 后原子替换 `nyan.exe`。
- [x] 在 Windows runner 至少运行一组定向 runtime smoke：Git Bash shell、PowerShell tool、`rg.exe --version`、Global Search、超时/杀进程树、路径转换。（#28、#31）

### P1：首发后尽快补齐

- [x] Windows RSS 通过 `GetProcessMemoryInfo` 实现，避免 `process_rss_bytes()` 恒为 0。（#27）
- [x] 用 Job Object 替代仅依赖 `taskkill /T /F` 的进程树管理，并验证后台任务、超时、取消和孙进程。（#31）
- [ ] 补 Windows ACL、junction/symlink、文件锁和共享删除的测试（部分完成，#30、#35、#37）。
  - 已有：junction 不能逃逸工作目录（`src/utils/permissions/filesystem.rs`）；junction/symlink 解析（`windows_link_tests.rs`）；独占句柄下 Write/Edit 报错且不截断（`file_write_tool`、`file_edit_tool`）；共享删除 unlink/rename（`src/utils/fs_operations/tests.rs:191`，用 `share_mode(0x7)` 即 FILE_SHARE_DELETE）；Windows 凭据文件 owner-only DACL 生效断言（`SE_DACL_PROTECTED`、单用户 ACE、access mask 与 SID 校验，#37）。
  - 缺口：共享删除只测允许删除/重命名成功的路径，未测被拒绝路径；file symlink 测试在无 SeCreateSymbolicLinkPrivilege 的 runner 上直接跳过（只打印原因），不算覆盖。
- [x] 为 Windows 增加 release zip、SHA256、可选签名和 SmartScreen 限制说明；实现后再接自动更新。
  - 决定（2026-11-03，Yuki 已定）：不签名，靠 SHA256 校验 + SmartScreen 说明。
  - 已完成：release.yml 产出 zip 与 `SHA256SUMS`；README 写明校验与 SmartScreen 警告（#32）。
  - 2026-11-09 更新：Windows 自动更新已实现，取代“暂不做、提示手动下载”的旧结论；仍然不签名。

### 明确不列入“最小可用”

- seccomp/bwrap/Seatbelt 的 Windows 等价实现：这是独立的安全产品级工作，未完成前只能降级并提示，不能用普通执行替代。
- 完整 ConPTY：如果产品明确只支持非交互 Bash 命令，可先以 pipe + 交互提示降级；需要全量终端程序支持时再单独立项。
