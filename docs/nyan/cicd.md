# CI/CD 设计（GitHub Actions）

状态：Phase 0/1 已实现并合并（2026-10-29）；Phase 2（nightly）/Phase 3（release）仍是设计。

设计来源：nyan-legacy 的 CI/CD——`.github/workflows/{ci,native,nightly,release,cache-prune,npm-audit,pr-describe,ocr-review}.yml`、
`.github/quality/policy.json`、`scripts/ci-{impact,shards,gate}.mjs`，以及两份 process note
（`2026-10-17-hosted-parallel-cicd.md`、`2026-10-19-test-quality-and-review-pipeline.md`）。取它的结论与已验证的
心智模型（hosted-only、一次构建分片执行、fail-closed 门禁、SHA 固定、发布已验证的字节），按 Rust 单 crate 的形态
重做；形态不同的部分逐条写明原因。

**定位**：nyan 是单 crate 的 Rust 二进制，编译是全 crate 的，测试必须走 nextest。legacy 的“选包 + 并行分片”在这里
只保留后半：**构建一次、分片执行**；按改动选测试不省编译时间（`cargo nextest archive` 仍要编译全部依赖和整个
crate）。本设计的中心是**一个门禁、一次构建、零 secret、不伪装红色**。

**最先要说明的事实**：当前 Linux 基线是红的。master `f9e624f` 在干净环境实测 `6633 tests run: 6607 passed,
26 failed, 5 skipped`。门禁不能带着 26 个已知失败上线；**先修绿是启用 required check 的前置条件**（第十节 Phase 0），
不是 CI 的运行时配置。第八节之前的所有设计都以“套件可绿”为前提。

## 一、现状（实测）

### 1.1 仓库形态

| 项 | 值 |
|---|---|
| crate | 单 crate `nyan`（非 workspace）；`autobins = false`，bin 是 `src/bin/nyan.rs`，构建目标是 `--bin nyan` |
| 规模 | 1438 个 `.rs`、约 67.8 万行、`src/` 32 MB |
| 测试 | 6633 个 nextest 测试，全部在 lib 的 `#[cfg(test)]` 模块里；`tests/` 只有 39 个 fixture 文件，没有集成测试 |
| 依赖 | 469 个锁定包；7 条直接 git 依赖，来自 3 个仓库（CometixTUI ×2、anthropic-sdk-rs ×4、marked-rs ×1；`Cargo.lock` 里 8 个 git 包），锁文件固定 rev |
| 工具链 | `rust-toolchain.toml` 钉 1.99.1（`rustfmt` + `clippy`，minimal）；`rust-version = "1.99"`（edition 2024）。设计时是 1.89.0 / MSRV 1.88，2026-10-29 统一升级 |
| 需要 C 工具链的依赖 | `onig_sys`、`ring`、`tree-sitter`、`tree-sitter-bash`（README.md:76） |
| CI 起点（设计时） | 无 `.github/`。origin 无 tag；master 未保护；SHA pinning 未强制；0 secret、0 variable、cache 使用 0 |
| CI 现状（2026-10-29） | `ci.yml` + quality policy 已合并；master 要求 PR 与 `Merge gate`；SHA pinning 已强制；0 secret |
| 上游 | `Haleclipse/CometixCode` 也没有 `.github/`（API 404），没有可继承的流水线 |

测试必须走 nextest，理由已写在 justfile 头部：`cargo test` 把 6600+ 个测试当一个进程里的线程跑，164 个文件调用
`set_var`、200 个共享 `TEST_ENV_LOCK`，互相污染；实测 `cargo test --lib` 有 530 个失败，`cargo nextest run --lib`
只有 23 个（justfile:1-15）。`just test` 另外钉住宿主状态（见 5.2）。

### 1.2 构建与测试实测

本机 24 核 / 62 GB，2026-10-27，`Cargo.lock` 未变：

| 操作 | 结果 | 备注 |
|---|---|---|
| `cargo nextest archive --lib`（冷，默认 debug=2） | 7 m 42 s，252 MB 归档 | 一次构建的完整成本 |
| 同上，`CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0` | 5 m 42 s，86 MB 归档 | CI 采用 |
| 同上，4 并发 + `CARGO_PROFILE_TEST_DEBUG=1` | 6 m 32 s，149 MB 归档 | 最接近 hosted 4 vCPU 的形状 |
| 仅改 `src/lib.rs` 后重建归档 | 1 m 41 s | 增量对 CI 无意义（runner 是新的） |
| `target/debug` 体积 | debug=2 约 7 GB；debug=1 2.3 GB；debug=0 1.4 GB | hosted runner 只有 14 GB SSD |
| 归档解包（86 MB） | 0.7 s，展开 338 MB | 分片 job 的固定成本 |
| 全量测试，默认线程（24） | 53 s | 6600+ 测试，本机 |
| 全量测试，`-j 4` | 167 s | 4 核形状的近似 |
| 一个 `hash:1/4` 分片，`-j 4` | 37 s（1647 个测试） | 分片均衡 |
| `cargo clippy --lib --bins` | 2 m 54 s（依赖已热），0 error，1623 warning | 可作为门禁 |
| `cargo clippy --lib --tests` | 1 个 correctness error（`src/services/mcp/client.rs:6432`，loop never loops），2669 warning | Phase 0 修掉后可按 correctness 门禁 |
| `cargo fmt --check` | 失败：master 447 个 hunk / 154 个文件（分支 HEAD 450 / 156） | 上游本身未格式化，见 4.4 |
| `cargo test --doc` | 4 m 06 s，17 ignored、0 passed | doctest 全被忽略，PR 不跑 |
| `cargo build --release --bin nyan` | 5 m 27 s，52 MB | strip + thin LTO + panic=abort |
| `~/.cargo/{registry,git}` | 约 522 MB | 可整体缓存（整个 `~/.cargo` 是 576 MB，含 bin） |

hosted 4 vCPU 的单核比本机慢（legacy 在 nyan-dev-01 上测得约 25%），上表只能当量级；**首次 hosted 运行必须记录
真实耗时并以实测修正第九节的目标**，不能把估计写进 policy 当结论。

### 1.3 基线是红的

master `f9e624f`，干净环境（无宿主模型变量、独立 HOME、PATH 含 Node 24，与 runner 形状一致）：

```
Summary [ 50.6s] 6633 tests run: 6607 passed, 26 failed, 5 skipped
```

宿主环境（父会话注入 `ANTHROPIC_*`）下是 31 个失败；多出的 5 个在干净环境通过，但只在宿主环境出现——既可能受
宿主变量影响，也可能受并发负载影响，Phase 0 要逐条判定，不能先假定是 flake：

- `components::prompt_input::tests::prompt_input_arrows_matches_official_cursor_before_history`
- `components::prompt_input::tests::prompt_input_vim_delete_operator_removes_logical_line`
- `components::settings::config::tests::config_language_free_text_submit_updates_preview_in_memory_only`
- `services::tools::tool_execution::executor_tests::local_tool_executor_runs_background_bash_and_task_output`
- `tools::bash_tool::tests::file_mode_returns_on_shell_exit_without_waiting_for_grandchild_fd`

26 个稳定失败按 panic 特征大致分四类（完整清单见附录 A；归类是初判，Phase 0 逐条确认）：

| 类 | 数量 | 特征 |
|---|---|---|
| 平台字形 | 4 | 断言 `⏺`，Linux 上 `BLACK_CIRCLE = "●"`（`src/constants/figures.rs:1049-1053`） |
| 缺 Tokio runtime | 6 | `use_typeahead.rs:1541` 在同步测试里 `spawn_blocking`；`in_child_process` 的子进程级联失败 |
| 渲染/快照 | 13 | canvas 文本、重渲染次数、picker 状态断言 |
| API/设置状态 | 3 | `services/api/claude` 的 system prompt 断言 |

`docs/nyan/README.md:77-79` 已记录 4 个 prompt_input 基线失败；本机干净环境只复现其中 2 个，说明它们同时受负载影响。
**这 26 个不是 CI 引入的**：它们在本机直接跑 `just test` 就存在。但 CI 的第一职责是把“绿”变成有意义的信号，
所以 Phase 0 必须先处理掉，而不是在门禁里加排除名单。

## 二、目标与非目标

### 目标

1. **每个 PR 一个可信的结果**：fail-closed；构建/测试/静态/供应链任一失败都阻塞；跳过只在被策略允许时算成功。
2. **一次构建、分片执行**：`build` 产出 `cargo nextest archive`，测试分片只下载、解包、跑；没有第二个 job 重新编译。
3. **PR p95 目标 20 分钟**（先定，首跑后按实测收紧）。关键路径是 build，不是测试。
4. **发布已验证的字节**：release 只接受“同一 commit 在 master 上过一次绿门禁”的 tag；构建一次、attest、发布，不重建。
5. **零 secret**：整条流水线不需要任何仓库 secret；fork PR 不接触凭据或自托管主机。
6. **全部 GitHub-hosted**：仓库是 public，标准 runner 免费且不限量（20 并发、macOS 最多 5）；不依赖 nyan-pc / corp-pc，
   也就没有“corp-pc 关机”和“公共仓库跑 fork 代码在自托管上”的问题。

### 非目标（首批；理由见第十一节）

自托管 runner、AI review（PR-Agent / OCR）、npm/crates.io 发布、全仓 `cargo fmt` 门禁、按改动选测试、
mutation/fuzz/coverage、merge queue、缓存清理工作流、代码签名/notarization。

## 三、总体形态

### 3.1 工作流清单

| 文件 | 触发 | 内容 | 阻塞 PR |
|---|---|---|---|
| `ci.yml` | `pull_request`、`push: master`、`merge_group`、dispatch | policy、build、static、tests×4、supply-chain、windows-check、Merge gate | 是 |
| `pr-describe.yml` | `pull_request`、dispatch | PR-Agent `/describe` 生成结构化变更摘要（sticky comment） | 否（建议性） |
| `ocr-review.yml` | `schedule`（18:30 UTC）、dispatch | 批量 AI 代码深度审查（OCR），对 master 提交进行阶段性巡检并开 Issue | 否（建议性） |
| `nightly.yml` | `schedule`（02:17 UTC）、dispatch | 全量 + internal feature + doctest、Windows check、fmt 漂移报告、全量安全扫描 | 否（失败要修） |
| `release.yml` | tag `nyan-v*.*.*`、dispatch（仅 dry run） | 前置校验、按平台构建、vendored ripgrep、冒烟、SHA256SUMS、GitHub Release | —— |

### 3.2 runner 放置

| 用途 | label | 理由 |
|---|---|---|
| PR / push 全部门禁作业 | `ubuntu-24.04` | public repo 免费且不限量；4 vCPU / 16 GB / 14 GB SSD |
| Linux arm64 发布构建 | `ubuntu-24.04-arm` | 原生构建，不交叉 |
| Windows 发布构建与冒烟 | `windows-2025` | 原生 MSVC 构建并跑 `--version`、bundled `rg.exe --version`（8.2） |
| AI 审查与摘要（PR-Agent / OCR） | `[self-hosted, Linux, X64, repo-nyan]` | 需要连通内网模型网关/OCR 调度器；严格限制 fork PR 不执行 |

固定具体 label 而不是 `-latest`：`ubuntu-latest` 将在 2026 年 11 月切到 26.04（runner-images 公告），工具链和
系统库会跟着漂移；Rust 构建对链接器和 glibc 敏感。`ubuntu-24.04` 的 apt 列表里没有 `ripgrep`、`zsh`——测试 job
需要 `rg` 在 PATH 上（与开发机一致），由 setup action 安装固定版本；`zsh` 缺失时测试自己跳过（`src/utils/shell/mod.rs:658`），
与开发机一致，不安装。`socat` 在镜像里也没有，但 `utils/sandbox/network_proxy.rs` 的单元测试直接启动它，
所以测试 job 用 apt 装 `socat`（`bubblewrap` 不装：没有测试真的启动 bwrap）。
镜像自带的 Node 是 22.23.3，开发机是 24.3.0；MCP / LSP 测试会启动 `node` 假服务器，
所以测试 job 用 `actions/setup-node` 钉到 24，不为版本差异留悬念。

### 3.3 作业图

```
policy ─┬─ build ──────── tests (hash:1/4 … 4/4)
        ├─ static
        ├─ supply-chain
        └─ windows-check (conditional)
                    └──────────── Merge gate (always)
```

`build` 与 `static` 并行，二者都不依赖对方；`tests` 只依赖 `build`；`gate` 等全部。docs-only 时只有 `policy` 与
`gate` 真正执行，其余被 `if` 跳过，`gate` 按 `docs_only` 放行。

### 3.5 分支模型与合并策略

面向“单人 + 多 AI 并行分支”的形态，规则刻意少而硬：

| 规则 | 设置 | 理由 |
|---|---|---|
| 唯一长分支 | `master` | 没有 develop/release 分支：单人项目多一条长期分支只会多一次同步 |
| 主题分支短命 | `<type>/<topic>`（feat/fix/refactor/chore/docs） | 一个分支一个可合并单元；合并即删 |
| 所有改动走 PR | branch protection 要求 PR，0 审批 | 门禁替代人审；PR 是“这次改动的 CI 结果”的载体 |
| 只允许 squash 合并 | 关闭 merge commit 与 rebase 合并 | master 每个 PR 一个提交，历史线性、可 bisect |
| 自动合并 | 仓库开启 auto-merge | 开完 PR 立刻 `gh pr merge --auto --squash`，绿了自动落地，不用守着 |
| 合并即删分支 | `delete_branch_on_merge` | 并行分支多，残留分支会让 `git branch` 和 Actions 列表变噪音 |
| 允许 update branch | `allow_update_branch` | master 前进后一键把分支更新到最新，避免本地 rebase 冲突处理 |
| admin 可绕过 | `enforce_admins=false` | 单人的紧急出口；日常仍走 PR（绕过会留痕在 branch protection 之外） |
| 本地并行 | 每个分支一个 worktree（`~/Projects/.worktrees/nyan-<task>`） | 已在用；同一 checkout 不能同时开两个 AI 会话 |

配套的工作流行为：`concurrency` 按 PR 号分组、`cancel-in-progress`——同一 PR 连续 push 只保留最新一次，不同 PR
互不干扰。分支上可以随意 force push（保护只在 master）。

快节奏下的实用约定：

- PR 开出来就跑 CI，不攒大 PR；文档类改动走 docs-only 快速通道（只有 policy+gate）。
- 失败先看 JUnit artifact（`junit-shard-N`），不要重跑碰运气——重试关掉了，红的都是真红。
- 合并前把 PR body 的 Validation 段按实际结果更新（AI 会话之间的交接凭据）。

### 3.4 与 legacy 的取舍

| legacy 决策 | 本文 | 原因 |
|---|---|---|
| 按改动选包、展开分片 | 只保留 docs-only 快速通道和 Windows 条件；测试固定 4 分片 | Rust 编译全 crate，选测试不省编译；分片仍省测试墙钟 |
| prepare 打包 `node_modules` + `dist` | `build` 产出 nextest archive | 等价物；实测 86 MB / 0.7 s 解包 |
| node + bun 两套矩阵 | 一套 lib 测试 | 只有一个测试目标 |
| PR 全部 hosted，nightly 自托管 | 全部 hosted | 免费；没有专用主机的队列和信任问题 |
| npm 发布 + provenance | GitHub Release + build provenance | 不是 npm 包 |
| model-data 快照哈希 | `--locked` + 固定工具链/action/工具哈希 | 复现性的等价物（Cargo.lock 已是权威） |
| fail-closed Merge gate | 原样保留 | 核心 |
| 影响分析失败关闭 | 原样保留 | 核心 |
| AI review（describe / OCR） | 首批不做 | 需要网关和凭据；公共仓库的 fork 面 |
| 缓存清理 | 首批不做 | 只缓存 registry（<1 GB），离 10 GB 很远 |
| Windows 只做 build+check | Windows check + release 冒烟 | TUI 的 Windows 代码有 60 个文件带 `cfg(windows)`/`cfg!(windows)`，含 `cfg(not(unix))` 共 74 个 |

## 四、PR 门禁 `ci.yml`

### 4.0 触发、并发、权限

```yaml
on:
  push: { branches: [master] }
  pull_request: { branches: [master], types: [opened, synchronize, reopened, ready_for_review] }
  merge_group:
  workflow_dispatch:

concurrency:
  # PR 按号码归组、新推送取消旧 run。push / merge_group 用 run_id 归组，等价于“不并发取消”——
  # 否则同一 SHA 的 master run 可能被后一次 push 取消，release 前置校验就查不到它。
  group: ${{ github.event_name == 'pull_request' && format('ci-pr-{0}', github.event.pull_request.number) || format('ci-run-{0}', github.run_id) }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

permissions:
  contents: read
```

不用 `paths-ignore`：被跳过的 workflow 不会给 required check 上报结果，PR 会永远 pending。docs-only 由 `policy`
判定并在 `gate` 放行。不用 `pull_request_target`：本设计没有需要在 base 上下文里跑的东西，而它会把 fork 代码和
写权限放在同一上下文里。

### 4.1 `policy`（<1 分钟）

`ubuntu-24.04`，`fetch-depth: 0`，`actions/setup-node`（24，只跑脚本），只做判定，不安装依赖：

- `scripts/ci/impact.mjs` 输出：
  - `docs_only`：所有改动路径都在 `docs/**`、`**/*.md`、`assets/**`、`LICENSE`、`README*` 内。`.github/**`、
    `scripts/**`、`Cargo.*` 一律视为代码。
  - `windows_required`：`Cargo.toml` / `Cargo.lock` / `rust-toolchain.toml` 变更，或 diff 里出现新增/删除的
    `cfg(... windows ...)`、`target_os = "windows"`、`cfg(not(unix))` 行，或 policy 列出的路径。这是廉价代理：
    Windows 相关代码散在 60 个文件里，按目录列不准。
  - `test_matrix`：`[{"name":"shard-1","partition":"hash:1/4"}, …]`，数量来自 `policy.json` 的 `testShards`（首批 4）。
    `name` 不能含 `/`：artifact 名不允许 `/`（actions/toolkit 的 `invalidArtifactNameCharacters`），所以 shard 名与
    partition 分开。
- 基线用三点 diff：`git diff --name-only "$BASE...$HEAD"`。三个事件来源分别是
  `pull_request.base.sha` / `pull_request.head.sha`、push 的 `before` / `after`、`merge_group.base_sha` / `head_sha`。
  `fetch-depth: 0` 保证两个 SHA 都在本地历史里；`before` 全 0（新建分支）或任一 SHA 为空时按全量处理。
- **fail closed**：脚本报错、拿不到 SHA、输出为空 → `docs_only=false`、`windows_required=true`。

注意 checkout 在 PR 上默认取的是合成 merge commit（`refs/pull/N/merge`），这正是要门禁的对象；按 `base...head`
算出的文件集与它一致。

### 4.2 `build`（关键路径）

```yaml
build:
  name: Build test archive
  needs: policy
  if: needs.policy.outputs.docs_only != 'true'
  runs-on: ubuntu-24.04
  timeout-minutes: 30
  steps:
    - uses: actions/checkout@<sha>            # v7.0.1，与 legacy 相同，实施时填完整 SHA
    - uses: ./.github/actions/setup-rust      # 工具链 + nextest，版本来自 policy/tools.lock
    - name: Restore registry cache
      uses: actions/cache/restore@<sha>
      with:
        path: |
          ~/.cargo/registry
          ~/.cargo/git
        key: cargo-registry-${{ hashFiles('Cargo.lock') }}
        restore-keys: cargo-registry-
    - name: Build the test archive
      env:
        CARGO_INCREMENTAL: '0'
        CARGO_PROFILE_TEST_DEBUG: '0'          # 7 GB → 1.4 GB target，归档 252 MB → 86 MB
      run: cargo nextest archive --locked --lib --archive-file nyan-tests.tar.zst
    - uses: actions/upload-artifact@<sha>
      with:
        name: nyan-tests-${{ github.sha }}
        path: nyan-tests.tar.zst
        compression-level: 0                   # 已是 zstd
        retention-days: 3
        if-no-files-found: error
    - name: Save registry cache (master only)
      # 同一 Cargo.lock 的条目已存在时 save 会报 "cache already exists"，那正是期望状态，所以不阻塞。
      if: github.event_name == 'push' && github.ref == 'refs/heads/master'
      continue-on-error: true
      uses: actions/cache/save@<sha>
      with:
        path: |
          ~/.cargo/registry
          ~/.cargo/git
        key: cargo-registry-${{ hashFiles('Cargo.lock') }}
```

要点：

- `--locked` 是硬规则。`Cargo.toml` 的 git 依赖没有 `rev`，锁文件才是权威；不加 `--locked`，上游分支移动会
  让同一 commit 构建出不同代码。
- `CARGO_PROFILE_TEST_DEBUG=0` 的代价：失败时 backtrace 没有行号。分片日志仍有 panic 消息、测试名和
  `file:line` 断言位置；要行号时本地用默认 profile 复现。换来的构建时间和 14 GB 磁盘余量更重要。
- 缓存只保存 registry/git，不缓存 `target`：debug=2 的 target 约 7 GB，加上 registry、工具链和 checkout 会顶到
  hosted 的 14 GB SSD；且 target 里的 path 依赖和 git 依赖指纹很容易失效，命中率不值得。
- 缓存只在 master push 写：PR 的缓存条目按 ref 隔离，写了也没人复用，只会挤出 master 的条目（legacy 的教训）。
- 构建和测试 job 不要给 checkout 传 `path:`：`--workspace-remap .` 只重映射二进制位置，`env!("CARGO_MANIFEST_DIR")`
  的编译期路径（17 处）不会跟着变，两边必须同为 `/home/runner/work/nyan/nyan`（hosted 默认）。

### 4.3 `tests`（4 分片）

```yaml
tests:
  name: Tests (${{ matrix.name }})
  needs: [policy, build]
  # build 失败时不再跑测试：失败的分片只会制造噪音，gate 仍会把 skipped 判成失败。
  if: needs.policy.outputs.docs_only != 'true' && needs.build.result == 'success'
  strategy:
    fail-fast: false
    max-parallel: 4
    matrix:
      include: ${{ fromJSON(needs.policy.outputs.test_matrix) }}
  runs-on: ubuntu-24.04
  timeout-minutes: 20
  steps:
    - uses: actions/checkout@<sha>
    - uses: ./.github/actions/setup-rust      # 只装工具链与 nextest，不编译
    - uses: actions/setup-node@<sha>       # Node 24：与开发机一致；MCP/LSP 的假服务器由 `node` 启动
      with: { node-version: '24' }
    - uses: ./.github/actions/setup-tools  # rg（Grep / GlobalSearch 的测试子进程要用），校验 sha256
    - run: sudo apt-get update -qq && sudo apt-get install -y -qq --no-install-recommends socat
      # network_proxy 的单元测试会启动 socat 桥；镜像没有预装
    - uses: actions/download-artifact@<sha>
      with: { name: 'nyan-tests-${{ github.sha }}' }
    - run: scripts/ci/test.sh --archive nyan-tests.tar.zst --partition '${{ matrix.partition }}'
      # `--profile ci` 由脚本统一加，junit 落在 target/nextest/ci/junit.xml
    - name: Upload JUnit on failure
      if: failure()
      uses: actions/upload-artifact@<sha>
      with: { name: 'junit-${{ matrix.name }}', path: target/nextest/ci/junit.xml, if-no-files-found: ignore, retention-days: 3 }
      # matrix.name 用 shard-1…shard-4；artifact 名不允许 `/`
```

`test.sh` 内部（与本地共用）：

```sh
python3 scripts/ci/prepare-test-home.py            # 替代 justfile 的 _prep
export TERM=xterm-256color COLUMNS=80 LINES=24     # hosted runner 无 TTY/TERM，否则尺寸为 0
unset CI                                           # 产品代码会读 CI；profile 已在上一步选定
rm -rf .ci-extract && mkdir -p .ci-extract         # nextest 不创建目录，也不肯覆盖已存在的目录
cargo nextest run --profile ci \
  --archive-file nyan-tests.tar.zst \
  --workspace-remap . --extract-to .ci-extract \
  --partition "$PARTITION" --no-fail-fast
```

`--extract-to` 的目录必须先存在（nextest 不会创建）。分片用 `hash:k/4`，按测试名哈希，增删测试不需要维护清单；
实测 1647/6633，均衡。`.config/nextest.toml` 新增 `[profile.ci]`（`fail-fast = false`、`retries = 0`，不自动重试
掩盖 flake）和 `[profile.ci.junit] path = "junit.xml"`（输出在 `target/nextest/ci/junit.xml`，成败都写，失败时上传）。

### 4.4 `static`（与 build 并行）

`ubuntu-24.04`，`./.github/actions/setup-rust` + `setup-tools`（`actionlint`），只 restore registry 缓存不 save：

```sh
cargo check --locked --lib --tests
cargo check --locked --lib --tests --features anthropic_internal
cargo clippy --locked --lib --bins -- -D clippy::correctness -D clippy::disallowed_methods
cargo clippy --locked --lib --tests -- -D clippy::correctness
actionlint   # 固定版本二进制，见 tools.lock.json
```

- `anthropic_internal` 的检查是 justfile:140-160 明确要求的事故面：按 justfile 2026-08-05 的测量，50 个
  `#[cfg(feature = "anthropic_internal")]` 站点在默认构建里根本不编译（当前代码 48 处）。feature 只作用于本 crate，
  依赖产物可复用，所以同一 job 里第二条 check 只多 crate 的时间。
- 生产 clippy 实测 0 error（1623 warning 不阻塞）；`-D clippy::disallowed_methods` 守住 `process_env` 载体和
  `utils::proxy` 这两个项目不变量。测试代码不 deny `disallowed_methods`：`cargo clippy --lib --tests` 的
  `disallowed_methods` 共 666 条，其中 648 条是既存的 `std::env::temp_dir`，只 deny correctness；
  master 上 `client.rs:6432` 的 1 个 correctness error 在 Phase 0 修掉。**分支现状另有 1 条生产违规**：
  `src/services/rerank.rs:130` 新建 `reqwest::Client::builder`（clippy.toml 已禁止），合并前必须改走
  `utils::proxy`，否则本条门禁直接红。
- **不做全仓 fmt 门禁**：master 上 `cargo fmt --check` 有 447 个 hunk / 154 个文件（分支 HEAD 450 / 156）。全仓格式化会与上游 merge
  产生巨大冲突，而项目约定是“只格式化自己的 hunk”（docs/nyan/README.md:80）。nightly 只报差异计数，不阻塞。
- `actionlint` 检查 workflow 本身；workflow 的真正验证只能在 GitHub 上跑（legacy 的说法）。

### 4.5 `supply-chain`

- `gitleaks`（固定版本二进制 + `.gitleaks.toml`）扫描 `git archive HEAD` 的快照，不扫历史。**实测基线就有 9 条命中**，
  PR 从第一天起会红，Phase 0 必须先把它们写进 allowlist（按 rule + path；带行号的条目不随行号漂移）：
  3 条 `private-key` 是测试用自签证书 `tests/fixtures/tls/{client,client-encrypted,other}.key`；2 条
  `aws-access-token` 在 `src/components/feedback.rs:471,477` 的脱敏用例里（`AKIA…` 合成值）；3 条
  `generic-api-key` 在 `src/utils/fs_operations/path/UPSTREAM.json:19` 的文件哈希、`src/utils/mcp_websocket_transport.rs:266`
  的 RFC 6455 示例 key `dGhlIHNhbXBsZSBub25jZQ==`、`src/services/decision/egress.rs:267` 的脱敏夹具；1 条 `jwt` 在
  `src/services/decision/egress.rs:214` 的脱敏夹具。合成值优先加 `gitleaks:allow` 行内注释，测试证书按路径 allow。
- `cargo-deny --locked check`：advisories、bans、licenses、sources。git 源要显式允许三个
  `github.com/Haleclipse/*`；licenses 允许表按 Phase 0 实测生成（项目自身 AGPL-3.0-only）。
- 两者都是确定性证据，PR 阻塞。

### 4.6 `windows-check`（条件）

```yaml
windows-check:
  needs: policy
  if: needs.policy.outputs.docs_only != 'true' && needs.policy.outputs.windows_required == 'true'
  runs-on: ubuntu-24.04
  timeout-minutes: 30
```

命令：

```sh
rustup target add x86_64-pc-windows-gnu
sudo apt-get update && sudo apt-get install -y gcc-mingw-w64-x86-64
cargo check --locked --lib --tests --target x86_64-pc-windows-gnu
```

README.md:69-78 记录的就是这条路径（minGW 静态链接，`.exe` 不带 DLL）。首次运行要实测耗时并确认 `onig_sys` /
`ring` / `tree-sitter` 在 GNU 目标下可编译；如果失败，退回到 `windows-latest` 原生 check（成本更高，但不影响
关键路径）。

### 4.7 `Merge gate`

`if: always()`，只做判定，不重新跑工作：

```yaml
gate:
  name: Merge gate
  needs: [policy, build, static, tests, supply-chain, windows-check]
  if: always()
  runs-on: ubuntu-24.04
  timeout-minutes: 5
  steps:
    - uses: actions/checkout@<sha>
    - uses: actions/setup-node@<sha>
      with: { node-version: '24' }
    - run: node scripts/ci/gate.mjs
      env:
        CI_GATE_RESULTS: >-
          {"policy":"${{ needs.policy.result }}","build":"${{ needs.build.result }}",
           "static":"${{ needs.static.result }}","tests":"${{ needs.tests.result }}",
           "supply-chain":"${{ needs.supply-chain.result }}",
           "windows-check":"${{ needs.windows-check.result }}"}
        CI_GATE_IMPACT: >-
          {"docs_only":"${{ needs.policy.outputs.docs_only }}",
           "windows_required":"${{ needs.policy.outputs.windows_required }}"}
```

规则（`scripts/ci/gate.mjs`，必需列表来自 policy，不在这里抄第二份）：

- `policy` 必须 success；否则 fail（拿不到 impact 时不能把条件作业当通过）。
- `required` 中的 job：`success`；`docs_only == true` 时允许 `skipped`。
- `conditionalRequired` 的 job：flag 为 true 时 `success`，false 时允许 `skipped`；flag 缺失按 true 处理。
- `CI_GATE_RESULTS` 里缺少某个必需 job → fail（改名/删 job 会被抓到）。
- 其它情况一律 fail。

分支保护只要求这一个 check。job 改名不会造成“永久 pending”：门禁自己会报错。

### 4.8 docs-only 的行为

`docs_only=true`：`build/static/tests/supply-chain/windows-check` 全部 `if` 跳过，`gate` 只要求 `policy`。push 到
master 的 docs-only 提交同样只产生 policy+gate 的绿 run；release 前置校验（8.1）按 SHA 查询这次 run，接受它——
代码没有变，上一份代码的绿门禁仍然成立。

## 五、测试执行

### 5.1 单一来源

现在的环境钉住和 `_prep` 内联在 justfile 里（`_prep` 92-102、`iso` 75、`unhost` 86、`test` 122-124）。CI 如果复制
一份，两边必然漂移。Phase 0 把它们搬进：

- `scripts/ci/prepare-test-home.py`：重建 `target/test-home`，复制 `credentials.mock.json`，写带
  `hasTrustDialogAccepted` 的 `state.json`。
- `scripts/ci/test.sh`：调用上面的脚本、导出钉住变量、拼 nextest 命令；`--archive`/`--partition` 可选。
- justfile 的 `_prep`、`test`、`t`、`test-one`、`test-ant` 改成调用同一个脚本，行为不变。

这样 `just test` 仍然是开发机的唯一入口，CI 调用同一份逻辑。

### 5.2 环境钉住（三层）

| 轴 | 变量 | 原因 |
|---|---|---|
| 设置根 | `NYAN_CONFIG_DIR=target/test-home` | 同时重定向 settings 与其中的 `state.json` |
| 隐私/测试开关 | `NYAN_DISABLE_NONESSENTIAL_TRAFFIC=`、`NODE_ENV=` | 置空，避免宿主开关关掉错误日志/更新检查或切到 `NODE_ENV=test` 捷径 |
| cwd | `CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR=` | 否则 `reset_cwd_if_outside_project` 永远报项目根 |
| 项目设置 | `NYAN_TEST_PROJECT_DIR=tests/fixtures/isolated-project` | 补掉 `projectSettings` 读本仓库 `.nyan/settings.json` 的洞 |
| 宿主模型/网关 | `env -u` 掉 19 个 `ANTHROPIC_*` / `CLAUDE_*` 变量 | 空字符串会被当成模型名（justfile:86） |
| 身份 | `tests/fixtures/credentials.mock.json` | 合成 token，固定登录态 |
| 终端尺寸 | `TERM=xterm-256color`、`COLUMNS=80`、`LINES=24` | **CI 新增**：hosted runner 没有 TTY 也没有 `TERM`，crossterm 的 `window_size()` 失败后退到 `tput`，`tput` 因缺 `TERM` 再失败，`terminal::size()` 变成 `(0,0)`——Pane 分隔线、logo、spinner 计时、picker 描述宽度全部塌成 0。钉成开发机实测值 |
| CI 标志 | 进测试进程前 `unset CI` | 产品代码本身会读 `CI`（`auth.rs:1016` 跳过 API key 审批，`env.rs:87` 记录 `is_ci`）；hosted runner 全程 `CI=true`，会让 auth/state 断言走另一条分支。nextest profile 在 unset 前已决定 |
| locale/TZ | `LANG=C.UTF-8`、`TZ=UTC` | **CI 新增**，让排序和时间断言确定；首跑若出现 locale 敏感失败，修测试（显式 locale）而不是改 CI 变量 |

`TERM` 只钉 `xterm-256color`：`TERM=linux` 会让 `is_unicode_supported()` 返回 false（figures.rs:961-963），
改变整套字形。也不装 `zsh`：测试在缺失时自行跳过。

### 5.3 分片与执行

- 4 个 `hash:k/4` 分片，`max-parallel: 4`；账户上限 20 个并发 job，整条流水线最多约 9 个同时活着。
- 分片 job 只做三件事：下载 86 MB 归档、解包（0.7 s）、跑。没有 cargo 编译。
- `--no-fail-fast` + profile 里 `fail-fast=false`：一个分片里的失败不掩盖其它测试。
- 不做自动重试。失败的测试要么是产品缺陷、要么是 fixture/harness 缺陷；重试只会把它们藏起来（justfile:211-212
  的立场）。

### 5.4 基线处理

不在门禁里内置排除名单。Phase 0 的目标是附录 A 的 26 个在 Linux 上为 0；如果某个必须临时隔离，只能作为
“启用 required 之前的引导态”，并且同期不得把 Merge gate 设为 required——门禁一旦 required，就不允许有已知红灯。
nightly 会跑全量，隔离期也能看到进展。

## 六、安全与供应链

### 6.1 零 secret

整条流水线不需要任何 secret：

- PR 来自 fork 时，`pull_request` 不提供 secret；这里本来也没有。
- Release 不发布 npm，不需要 `NPM_TOKEN`；GitHub Release 用 `GITHUB_TOKEN` 和 `contents: write`。
- Provenance 用 OIDC，不需要密钥；但 attest job 需要三项权限：`contents: read`、`id-token: write`、
  `attestations: write`（缺第三项时上传 attestation 会被拒）。attest action 用 `actions/attest`（v4 的推荐入口），
  `attest-build-provenance` 只是它的包装。
- 没有自托管 runner，fork 代码不可能在持有 sudo / Docker / tailnet 的机器上执行。

### 6.2 action 与工具的固定

- 所有 `uses:` 固定到完整 commit SHA；repository settings 的 “Require actions to be pinned to a full-length
  commit SHA” 已打开（2026-10-29）。
- 第三方 action 沿用 legacy 已验证的版本（checkout v7.0.1、setup-node v7.1.0、upload/download-artifact v7、
  attest-build-provenance v4.2.2、cache v4.2.2）。**设计不编造 SHA**，实施时从各自 release 取。
- 非 action 工具（nextest、actionlint、gitleaks、cargo-deny、ripgrep）用 `.github/quality/tools.lock.json` 固定
  `版本 + url + sha256`，`scripts/ci/fetch-tool.mjs` 下载校验；版本也写进 policy，两处不一致即失败。
  `cargo-deny` 与 `gitleaks` 作为 GitHub Release 资产下载（不是 `cargo install`，避免每次编译）；ripgrep 取
  BurntSushi/ripgrep 的官方 release，Linux 用 musl 静态构建。
- Dependabot：`cargo` 与 `github-actions` 每周。git 依赖 Dependabot 不管，`cargo update` 由 owner 手动做。

### 6.3 仓库设置（已在 2026-10-29 落地）

| 设置 | 值 | 状态 |
|---|---|---|
| Branch protection（master） | 要求 PR；require `Merge gate`；禁止 force push / 删除；admin 可绕过 | 已开（提交 `71f574c` 后） |
| 合并方式 | 只允许 squash；开启 auto-merge；合并后删分支；允许 update branch | 已开 |
| Actions SHA pinning | required（`sha_pinning_required=true`） | 已开；下次改 workflow 才会真正校验 |
| Workflow 默认权限 | read-only；不允许 Actions 批准 PR | 已开 |
| Dependabot alerts / security updates | 开启 | 已开 |
| Environment `release` | 只允许 `nyan-v*` tag 部署 | Phase 3 |
| Dependabot 版本更新 | `cargo` + `github-actions` 每周 | Phase 2 |

SHA pinning 的取舍（单人项目的成本收益）：

- **开**：它挡的是“tag 被重新指向别的 commit”这一类供应链攻击，开关是零维护的一次性设置；本设计的 `uses:`
  本来就全部写完整 SHA，开启只是让规则可执行，不会有迁移成本。已经用一次 dispatch（run 40848825058）验证
  开启后全绿。
- **不开的理由不成立**：唯一的实际代价是升级 action 时要手工换 SHA，而 Dependabot 的 `github-actions` 更新会
  直接把 SHA 版本写好，这个代价也被消掉了。
- **仍需人工判断的**：Dependabot 提的 action 升级要读一眼 release notes；`cargo` 的更新按测试结果决定。

## 七、Nightly

`schedule: 17 2 * * *`，`workflow_dispatch`，全部 hosted，`concurrency` 不取消。失败即红，但不阻塞 PR。

| job | 内容 | 目的 |
|---|---|---|
| full | `build` + 单 job 跑全量（4 核约 3 分钟测试） | 每日确认主分支绿 |
| internal | `nextest archive --features anthropic_internal` + 全量 | justfile 测得 29 个只在 feature 下的测试 |
| doctest | `cargo test --doc` | 当前 17 ignored、0 passed，成本 4 分钟，只放夜间 |
| windows | `cargo check --target x86_64-pc-windows-gnu` 或 `windows-latest` 原生 check | 不在 PR 关键路径 |
| fmt-drift | `cargo fmt --check` 的差异计数写进 job summary | 不阻塞，防止数字悄悄变大 |
| security | gitleaks + cargo-deny + SBOM（`cargo cyclonedx` 或 syft） | 每日全量；PR 只跑 gitleaks/deny |
| release-profile（可选，后续） | `cargo nextest run --cargo-profile release` 的单分片 | 捕捉只存在于 release 配置的问题 |

mutation（`cargo-mutants`）与 fuzz 不在首批：先在 Phase 0 把基线修绿，再挑 context-governance 的一个模块
（例如压缩的掩码选择）做小范围 mutation，阈值由第一轮实测决定，不预设分数。

## 八、Release

### 8.1 tag 与前置校验

- tag 形式 `nyan-vX.Y.Z`，可带预发布后缀（`nyan-v0.2.1-rc.1` → `--prerelease`）。origin 当前没有 tag，不存在
  与上游冲突的问题；沿用 legacy 的 `nyan-v` 前缀，与项目名一致。`Cargo.toml` 当前是 `0.2.0`，所以首次正式发布
  的 tag 是 `nyan-v0.2.0`；Phase 3 的预发布示例必须与当时的 `Cargo.toml` 一致，否则会被 §8.1 的第二条校验拒绝。
- tag 的版本必须等于 `Cargo.toml` 的 `version`（`scripts/ci/check-version.mjs`，预发布后缀只比较 `X.Y.Z` 主干）。
- **同一 commit 必须有一次成功的 master push 门禁**：用 `gh run list --workflow=ci.yml --commit <sha> --event push
  --branch master --status success` 查；查不到就失败，并提示“先等门禁跑完”。这比在 release 里重跑测试便宜，也
  保证发布的是通过同一套检查的代码（legacy 的 P4 决策）。
- 校验通过前不构建任何东西。

### 8.2 构建矩阵

| 产物 | runner | 方式 | 冒烟位置 |
|---|---|---|---|
| linux-x64 | `ubuntu-24.04` | `cargo zigbuild --release --locked --bin nyan --target x86_64-unknown-linux-musl` | 同 job |
| linux-arm64 | `ubuntu-24.04-arm` | `cargo zigbuild --release --locked --bin nyan --target aarch64-unknown-linux-musl` | 同 job |
| windows-x64 | `windows-2025` | 原生 `cargo build --release --locked --bin nyan --target x86_64-pc-windows-msvc` | 同 job |

每个 job 构建一次，之后没有任何 job 重建。Linux 使用 `cargo-zigbuild` + Zig 0.14.1；Windows 在原生 MSVC runner
上构建。不发布 macOS 产物。

### 8.3 vendored ripgrep 与归档布局

`src/utils/ripgrep.rs:78-118` 的解析顺序是：可执行文件旁的 `vendor/ripgrep/<arch>-<os>/rg` → 源码树 → PATH。
当前发布包只在 Windows 归档中携带第一项；Linux 归档依赖宿主 `rg`，没有时 Grep/GlobalSearch 不可用。目录名由同一段代码定义：

| 平台 | vendor 目录 | ripgrep release 资产 |
|---|---|---|
| linux-x64 | `x64-linux` | `x86_64-unknown-linux-musl`（静态） |
| linux-arm64 | `arm64-linux` | `aarch64-unknown-linux-musl` |
| windows-x64 | `x64-win32` | `x86_64-pc-windows-msvc`（改名 `rg.exe`） |

ripgrep 版本固定在 release workflow（当前 15.2.0），下载 `.zip` 后按固定 SHA256 校验。
归档布局：

```
nyan-v0.2.0-linux-x86_64.tar.gz
  nyan
  LICENSE

nyan-v0.2.0-windows-x86_64.zip
  nyan.exe
  LICENSE
  vendor/ripgrep/x64-win32/rg.exe
  vendor/ripgrep/COPYING
  vendor/ripgrep/LICENSE-MIT
  vendor/ripgrep/UNLICENSE
```

Linux 是 tar.gz，Windows 是 zip。LICENSE（AGPL-3.0）必须随包。

### 8.4 冒烟、校验与发布

冒烟命令（每个平台）：

```sh
NYAN_CONFIG_DIR=$(mktemp -d) ./nyan --version    # 输出以版本号开头
NYAN_CONFIG_DIR=$(mktemp -d) ./nyan --help       # 退出码 0
vendor/ripgrep/<dir>/rg --version                     # 结构化验证 vendor 路径存在且可执行
```

`--version` 不需要模型；用临时 `NYAN_CONFIG_DIR` 避免写 runner 的 home。

- `assemble` job 汇总三个归档，用 `sha256sum` 生成 `SHA256SUMS`，并断言恰好三行。
- `publish` job 只在 tag push 时运行，创建 GitHub Release 并上传三个归档与 `SHA256SUMS`；Release 已有资产时直接
  失败，不带 `--clobber`，避免替换用户已经校验过的字节。
- `workflow_dispatch` 是 dry run：构建 + 冒烟 + 上传 artifact，不创建 Release。

### 8.5 已知限制（不伪装）

- **没有代码签名**：Windows 版二进制未签名，SmartScreen 首次运行会警告。发布说明要求先用 `SHA256SUMS` 校验，
  再按 SmartScreen 提示继续；不提供签名，也不发布 macOS 产物。

## 九、缓存、并发、时间预算

### 9.1 缓存

| 项 | 结论 |
|---|---|
| `~/.cargo/registry` + `~/.cargo/git` | 缓存；实测约 522 MB，key 为 `Cargo.lock` 哈希；恢复约 10–20 s |
| `target/` | 不缓存（1.4–7 GB，10 GB 配额，且指纹易失效） |
| 保存策略 | 只在 master push 保存；PR 只 restore（`restore-keys`） |
| 清理工作流 | 首批不做；当前缓存用量 0，单 key <1 GB |
| sccache | 后续实验。它是唯一能跨 run 复用依赖编译的手段，但要占 GHA cache（10 GB）且对工具链/flag 敏感；先测命中率和收益 |
| 并发 | PR 取消旧 run；push 不取消；release 不取消 |
| 超时 | build 30、static 25、tests 20、supply 10、windows 30、gate 5、release 每平台 40 |
| 产物保留 | archive 3 天；JUnit 3 天；release 资产 14 天（GitHub Release 永久） |

### 9.2 时间预算

| 阶段 | 本机实测（量级） | hosted 估计 | 说明 |
|---|---|---|---|
| policy | <10 s | <30 s | 无安装 |
| build | 5 m 42 s（24 核）/ 6 m 32 s（4 并发） | 8–12 min | 关键路径；首跑实测 |
| static | 热依赖 clippy 2 m 54 s；冷构建未单独测 | 8–12 min | 与 build 并行，不叠加 |
| tests ×4 | 37 s / 分片（本机 4 线程） | 2–4 min | 下载 + 解包 + 跑 |
| supply-chain | 未测 | 1–2 min | 与 build 并行 |
| gate | 数秒 | <30 s | |
| **合计（关键路径）** | —— | **约 12–18 min** | 目标 p95 20 min，首跑后按实测收紧或放宽 |

build 是唯一值得继续优化的点：减少 debuginfo 已经做了；下一步才是 sccache 或拆分 workspace（大改，不在本设计）。
不把“目标 10 分钟”抄过来——那是 TypeScript 的测量结果，Rust 冷构建做不到。

## 十、实施顺序

每一步以“Done when”的证据为准；未经授权不 commit / push / 改仓库设置。

### Phase 0：先把基线修绿（纯仓库改动，无 CI）

1. 用与 CI 完全相同的环境重测 master（命令见附录 A），逐条确认 26 个失败：修测试或修产品代码。字形类改成
   `figures::BLACK_CIRCLE` 或按 `cfg(target_os = "macos")` 门控；缺 runtime 的用 `#[tokio::test]` 或显式
   `Runtime::block_on`；渲染/排序类先断定是不是 locale/负载。
2. 修两处 clippy：`src/services/mcp/client.rs:6432` 的 correctness error（`loop` 从不循环），以及分支上新增的
   `src/services/rerank.rs:130`（`reqwest::Client::builder`，必须改走 `utils::proxy`）。
3. 写 `.gitleaks.toml`，按 4.5 的 9 条实测命中逐条 allow（合成值加行内 `gitleaks:allow`，测试证书按路径），
   直到 `gitleaks detect --no-git` 对 `git archive HEAD` 返回 0。
4. 加 `rust-toolchain.toml`（设计时 1.89.0，2026-10-29 升到 1.99.1）；加 `scripts/ci/{prepare-test-home.py,test.sh}`，justfile 改为委托；
   本地 `just test` 行为不变。
5. 加 `.config/nextest.toml`（`profile.ci` + junit）、`deny.toml`；本地跑通
   `cargo clippy` 两条门禁命令、`gitleaks`、`cargo deny check`、`actionlint`。
6. 加 `.github/quality/{policy.json,tools.lock.json}` 与 `scripts/ci/{impact.mjs,gate.mjs,check-version.mjs}`；
   用本地 git 仓库构造 docs-only、Windows 命中、脚本失败三种输入验证。

Done when：干净环境下 `6633 tests run: … 0 failed`；两处 clippy 修完，两条门禁命令 exit 0；`gitleaks` 0 命中；
`cargo deny check` 与 `actionlint` 通过；`just test` 与旧行为一致。

### Phase 1：PR 门禁

1. 加 `.github/workflows/ci.yml` 与 `.github/actions/setup-rust`。
2. 在一个 PR 上跑通；记录每个 job 的真实耗时，更新 9.2 与 policy 的 `targetP95Minutes`。
3. 打开 branch protection（要求 PR、只 require `Merge gate`）、squash-only 合并、auto-merge 和 SHA pinning。

已完成（2026-10-29）：PR [#2](https://github.com/blxzer77/nyan/pull/2) 合并为 `71f574c`；首跑 8m51s，第二次
8m36s，均全绿。设置项见 6.3。

Done when：docs-only PR 只跑 policy+gate 并绿；普通 PR 的 gate 在 20 分钟内给出结论；失败注入（改坏一个测试、
  改坏一个 workflow）分别被 tests 与 actionlint 抓到。

### Phase 2：Nightly

全量、internal feature、doctest、Windows check、fmt 漂移、SBOM。Done when：手动 dispatch 一次全绿；
  故意注入的失败能让 workflow 变红，且不影响 PR。

### Phase 3：Release

1. 加 `release.yml`：preflight、矩阵构建、vendor ripgrep、冒烟、attest、发布。
2. 先 `workflow_dispatch` dry run，确认五个平台的冒烟与归档布局。
3. 打一个预发布 tag（示例：先把 `Cargo.toml` bump 到 `0.3.0`，再打 `nyan-v0.3.0-rc.1`）走真实发布；确认
   provenance、SHA256SUMS、重跑 clobber。
4. owner 创建 `release` environment 并限制到 `nyan-v*` tag。

Done when：dry run 与一次真实预发布都成功；删除再重跑同一 tag 不会产生第二个 release。

### Phase 4（可选，按收益决定）

sccache 实验、release-profile 测试、小范围 mutation、coverage、merge queue、AI review（advisory）。

## 十一、明确不做

| 项 | 原因 |
|---|---|
| 自托管 runner | 公共仓库 + fork PR 的信任边界；hosted 免费且并发足够；本机/公司机器会关机 |
| 按改动选测试 | 单 crate，编译时间与跑哪些测试无关；只保留 docs-only 与 Windows 条件 |
| 全仓 `cargo fmt` 门禁 | master 上 447 个既存 hunk；全仓格式化与上游 merge 冲突；项目约定只格式化自己的 hunk |
| clippy 全 warning 门禁 | 1623 个既存 warning；只守 correctness 与 disallowed_methods |
| 自动重试 | 会掩盖真实缺陷；失败必须有人看 |
| 已知失败排除名单 | 门禁 required 后不允许红灯；Phase 0 修绿 |
| npm / crates.io 发布 | 不是库，也不是 npm 包；发行渠道是 GitHub Release |
| 代码签名 / notarization | 没有 Apple / Windows 签名证书；写进发布说明的已知限制 |
| AI review（PR-Agent / OCR） | 需要网关与凭据，且要先有稳定的确定性门禁；后续可作为 advisory |
| mutation / fuzz / coverage | 基线未绿、测试目标单一；先修绿再按模块加 |
| Trivy / Semgrep / CodeQL | cargo-deny + gitleaks 覆盖当前风险；代码扫描工具后续按需 |
| 缓存清理工作流 | 缓存 <1 GB，离 10 GB 很远；出现滥用再加 |
| merge queue | 单开发者、无并发合并压力；需要时 `merge_group` 已接好 |
| 自更新 / 安装器 | CI 只产出发布资产；安装器是另一个设计 |

## 附录 A：master 基线失败清单

测量命令（Phase 0 用它复测；`target/test-home` 由 `just _prep` 或脚本生成）：

```sh
just _prep
env NYAN_CONFIG_DIR="$PWD/target/test-home" \
    NYAN_DISABLE_NONESSENTIAL_TRAFFIC= NODE_ENV= \
    CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR= \
    NYAN_TEST_PROJECT_DIR="$PWD/tests/fixtures/isolated-project" \
    env -u ANTHROPIC_MODEL -u ANTHROPIC_SMALL_FAST_MODEL … -u ANTHROPIC_API_KEY \
    cargo nextest run --lib --no-fail-fast
```

master `f9e624f` 的 26 个（`src/utils/test_env.rs:147` 的 `in_child_process` 失败是子进程里重跑同一测试后的级联，
不单独归类）：

| 测试 | panic 位置 / 特征 | 初步归类 |
|---|---|---|
| `components::agents::agents_menu::tests::initial_mode_renders_resolved_list_and_footer` | agents_menu.rs:304，canvas 缺 footer | 渲染 |
| `components::log_selector::tests::{log_search_matches_…, preview_escape_…, progressive_load_more_…, rename_submit_…}` | log_selector.rs:2961/2566/1955/2460 | 渲染/状态 |
| `components::messages::assistant_tool_use_message::tests::{notebook_header_…, scoped_file_headers_…}` | `⏺` vs `●` | 平台字形 |
| `components::messages_list::tests::logo_header_memo_skips_idle_parent_rerenders` | 2 vs 1 次重渲染 | 计时 |
| `components::prompt_input::tests::{prompt_input_color_only_banner_…, prompt_input_renders_live_swarm_banner_…}` | 边框/banner canvas | 渲染 |
| `components::settings::config::tests::{config_language_picker_…, config_model_selection_…}` | config.rs:3585/3302 | 渲染/状态 |
| `screens::repl::tests::logo_header_hot_path_avoids_uncached_auth_io_…` | settings_file_reads 1 vs 0 | 状态 |
| `screens::repl::tests::model_command_selection_…` | 本地命令输出行 | 渲染 |
| `screens::repl::tests::resume_by_{ambiguous,exact,id,missing_arg}_…` | use_typeahead.rs:1541 no reactor | 缺 runtime |
| `screens::repl::tests::{resume_cross_project_…, resume_pending_clipboard_…}` | test_env.rs:147 子进程断言级联；子进程内 repl.rs:14465 未渲染 “Failed to resume” | 缺 runtime/渲染 |
| `screens::repl::tests::resume_picker_selection_error_…` | repl.rs:14465 未渲染 “Failed to resume” | 渲染 |
| `services::api::claude::tests::{sdk_params_skip_global_…, sdk_params_use_system_prompt_…, sdk_request_adapter_…}` | claude.rs:5914/5798/5853 | 设置状态 |
| `utils::conversation_recovery::tests::{ask_user_question_…, enter_exit_plan_mode_…}` | 断言以 `⏺` 开头 | 平台字形 |

负载敏感、在宿主环境出现的 5 个见 1.3；它们在本机干净环境通过，但 CI 的 4 核并发可能让它们重新出现，
Phase 0 要把“依赖时序”本身修掉（等待条件而不是 sleep/固定次数），并把宿主变量隔离交给 5.2 的钉住层。

## 附录 B：`.github/quality/policy.json` 草案

```json
{
  "schemaVersion": 1,
  "pullRequest": {
    "targetP95Minutes": 20,
    "testShards": 4,
    "impactAnalysisFailClosed": true,
    "required": ["policy", "build", "static", "tests", "supply-chain"],
    "conditionalRequired": { "windows-check": "windows_required" },
    "docsOnlyFastPath": true
  },
  "toolchain": { "rust": "1.99.1", "nextest": "0.9.150", "ripgrep": "15.2.0" },
  "runners": {
    "ubuntu": "ubuntu-24.04",
    "ubuntuArm": "ubuntu-24.04-arm",
    "windows": "windows-2025"
  },
  "windows": {
    "paths": ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"],
    "diffPatterns": ["cfg\\(.*windows", "target_os = \"windows\"", "not\\(unix\\)"]
  },
  "cache": { "registryMaxBytes": 1000000000, "maxAgeDays": 14 },
  "release": {
    "tagPrefix": "nyan-v",
    "requireSameCommitGate": true,
    "platforms": ["linux-x64", "linux-arm64", "windows-x64"]
  }
}
```

## 附录 C：测量记录

| 测量 | 命令 / 方法 | 结果 |
|---|---|---|
| 冷归档（24 核） | `cargo nextest archive --lib` | 7 m 42 s / 252 MB |
| 冷归档（debug=0） | `CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 …` | 5 m 42 s / 86 MB |
| 冷归档（4 并发，debug=1） | `CARGO_BUILD_JOBS=4 …` | 6 m 32 s / 149 MB |
| 增量重建 | `touch src/lib.rs && cargo nextest archive --lib` | 1 m 41 s |
| 全量测试 | `cargo nextest run --lib --no-fail-fast` | 53 s / 6633 |
| 全量测试（4 线程） | 同上 `-j 4` | 167 s |
| 单分片 | `--partition hash:1/4 -j 4` | 37 s / 1647 |
| 解包 | `tar -xf nyan-tests.tar.zst` | 0.7 s / 338 MB |
| clippy 生产 | `cargo clippy --lib --bins` | 0 error / 1623 warning / 2 m 54 s（热依赖） |
| clippy 测试 | `cargo clippy --lib --tests` | 1 correctness error / 2669 warning |
| fmt | `cargo fmt --check` | master 447 hunk / 154 文件 |
| doctest | `cargo test --doc` | 17 ignored / 0 passed / 4 m 06 s |
| release 构建 | `cargo build --release --bin nyan` | 5 m 27 s / 52 MB |
| 基线（master） | 附录 A 命令 | 6607 passed / 26 failed / 5 skipped |
| runner 规格 | GitHub 文档 | 4 vCPU / 16 GB / 14 GB SSD；public repo 免费；20 并发；macOS 5 |
