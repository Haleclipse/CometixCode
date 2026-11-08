# AGENTS.md (nyan 仓库规则)

面向本仓库 AI 代理开发的自动化与规范指南。所有在此代码库工作的 Agent 必须严格遵守。

## 1. 工具链与构建规则

- **Rust 工具链**：固定为 `1.99.1`（`edition 2024`，MSRV `rust-version = "1.99"`），不要随意更改 `rust-toolchain.toml` 或 `Cargo.toml` 的版本定义。
- **构建命令调用**：直接调用 `cargo` 或 `just`（由系统 `devguard` 限流排队），**严禁**绕过 devguard 调用底层二进制，**严禁**设置 `DEVGUARD_DISABLE=1`。
- **常用开发命令**：
  - `just check`：快速执行类型与借用检查（不运行测试，用于代码重构时的轻量反馈）。
  - `just check-ant`：检查开启 `anthropic_internal` feature 时的编译。
  - `scripts/ci/test.sh -E 'test(/<pattern>/)'`：精准运行匹配模式的单个或模块单测。
  - `just test`：运行完整本地单测套件（已内置环境隔离与终端尺寸钉住）。

## 2. 质量防御与测试红线

- **严禁弱化测试**：修复 Bug 或调整实现时，**严禁**通过给失败测试加 `#[ignore]`、删除有效断言或放宽校验来蒙混过关。CI 的 `policy` 门禁会自动拦截任何新增 `#[ignore]`。
- **遇到测试失败必须追溯根因**：多数假性失败源于进程全局状态（`process_env`、全局单例缓存、终端尺寸 `(80, 24)`、`LANG/TZ`、Mock 运行时），使用 `EnvVarGuard` 或 `PinnedProjectDir` 做隔离，切勿盲目修改业务预期。
- **代码整洁与安全**：
  - 严禁提交任何真实 API Key、私钥或凭证（CI 执行 `gitleaks` 零容忍扫描）。
  - 提交前保证 `actionlint`、`cargo deny check` 以及核心 clippy 规则无新增报错。

## 3. 分支模型与无人值守交付（Trunk-based + Auto-merge）

- **master 受保护**：master 是生产主线，要求全部改动必须通过 Pull Request 并通过 `Merge gate`。禁止直推 master，禁止 force-push master。
- **短命主题分支**：
  - 每次独立任务从最新 master 迁出分支，命名规则：`feat/*`、`fix/*`、`chore/*`、`docs/*`、`refactor/*`。
  - 本地多个并行任务在独立 worktree（`~/Projects/.worktrees/nyan-<task>`）中进行，互不干扰。
- **标准交付流程**：
  1. 本地完成开发与验证（`just check` + 模块定向单测）。
  2. 推送分支至远端：`git push -u origin <branch>`。
  3. 创建 PR 并附带清晰的 Conventional Commit 标题与变更清单：`gh pr create --title "..." --body "..."`。
  4. 开启无人值守自动合并：`gh pr merge --auto --squash`。
  5. 待 CI 全绿后 GitHub 自动执行 squash merge 并自动删除远端分支；本地切回 master 执行 `git pull --ff-only`，并清理本地临时分支/worktree。

## 4. GitHub Actions 供应链要求

- 仓库已开启 Actions SHA-pinning 强校验。所有 workflow 中的 `uses:` 必须指向 40 位完整 Commit SHA（如 `actions/checkout@3d3c42e5...`），严禁使用可变 tag（如 `@v4`）。
- 依赖项升级通过配置好的 Dependabot 自动化分组 PR 进行，合并同样走 auto-merge 流程。
