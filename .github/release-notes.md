## What's new in 0.2.2

**Breaking**

- The `statusLine` setting and the `/statusline` command are removed; the footer now shows the same information built in. A leftover `statusLine` key in `settings.json` is ignored on load but fails strict validation when the file is edited through nyan — delete it.

**New TUI**

- **Footer info line**, always on: directory, branch (`@sha` when detached), `◇` inside a linked worktree; model, thinking effort, context as `used/window` (e.g. `84k/200k`), cache read share of the latest request, and decode speed in tokens/s. Narrow terminals drop segments by priority instead of wrapping.
  - Context turns yellow 40k tokens before auto-compact and red at 20k; the old "Context low (N% remaining)" line is now a short hint.
  - The cache segment turns yellow when a request misses a prefix that should have been cached (not after compaction, `/clear`, `/resume` or a model switch).
  - tokens/s counts output (including thinking) from the first streamed token to the end of each main-thread request, summed per turn.
- **Todo panel**: the task list now shows under the spinner while working and above the prompt when idle — expanded, one line, or `✔ N/N done` until your next message. `Ctrl+T` expands or collapses it; collapsing by hand keeps it collapsed for the session.
- **Agent panel**, below the prompt: one row per background agent, teammate, shell or dream with its latest tool call, tool count, tokens and elapsed time. `↓` from an empty prompt focuses it — the selected row shows its last three tool calls, `Enter` opens its details, `x` stops it, `Esc` returns. Finished rows leave with a short `/tasks to see agents` hint; failed or stopped rows stay 30 seconds. It replaces the teammate tree under the spinner and the tasks/teammates pills (which come back on terminals under 16 rows).

**New**

- `nyan web search <query…>` and `nyan web fetch <url>` expose nyan's web search and fetch on the command line, with `--json`, `--limit`, `--range` and `--timeout`.

**Fixes**

- Very long sessions no longer render a blank screen: drawing coordinates are widened past 32767 rows, and thinking blocks are collapsed by default (`Ctrl+O` shows them; an explicit `expandThinking: true` still applies).
- Resuming an interrupted turn no longer injects "Continue from where you left off." — the prompt shows "Previous turn was interrupted · Enter to continue" instead.
- The code-search index no longer walks nested worktrees, virtualenvs and other ignored directories in non-git folders, which could exhaust memory; one index is shared per worktree.

## 0.2.2 更新内容

**不兼容变更**

- 移除 `statusLine` 设置与 `/statusline` 命令，页脚改为内置显示同类信息。`settings.json` 里残留的 `statusLine` 键加载时会被忽略，但经 nyan 编辑该文件时会被严格校验拒绝，请删除。

**全新 TUI**

- **页脚信息行**，常驻显示：目录、分支（detached 时为 `@sha`）、linked worktree 中显示 `◇`；模型、思考等级、上下文 `已用/窗口`（如 `84k/200k`）、最近一次请求的缓存读取占比、解码速度 t/s。窄屏按优先级省略各段，不换行。
  - 上下文在距自动压缩 40k 时变黄、20k 时变红；原来的「Context low (N% remaining)」改为短提示。
  - 本应命中缓存的前缀未命中时，cache 段变黄（压缩、`/clear`、`/resume` 或切换模型后的首个请求除外）。
  - t/s 统计主线程每个请求从首个流式 token 到结束的输出速度（含思考），按轮汇总。
- **todo 面板**：任务列表在生成中显示于 spinner 下方，空闲时显示于输入框上方；有展开、单行、`✔ N/N done` 三种形态，全部完成后保留到下一次提交。`Ctrl+T` 展开 / 收起；手动收起后本会话不再自动展开。
- **agent 面板**，位于输入框下方：每个后台 agent、teammate、shell、dream 一行，显示最近一次工具调用、工具数、token 与耗时。空输入框按 `↓` 进入——选中行展开最近 3 次工具调用，`Enter` 打开详情，`x` 停止，`Esc` 返回。完成的行立即消失并短暂提示 `/tasks to see agents`；失败或被停止的行保留 30 秒。它取代了 spinner 下的 teammate 树与 tasks / teammates 小标签（终端不足 16 行时小标签恢复）。

**新增**

- `nyan web search <query…>` 与 `nyan web fetch <url>`：在命令行调用 nyan 的网页搜索与抓取，支持 `--json`、`--limit`、`--range`、`--timeout`。

**修复**

- 超长会话不再白屏：绘制坐标突破 32767 行上限；thinking 块默认折叠（`Ctrl+O` 查看，显式设置 `expandThinking: true` 仍然生效）。
- 恢复被中断的回合时不再注入「Continue from where you left off.」，改为在输入框上方提示「Previous turn was interrupted · Enter to continue」。
- 代码检索索引在非 git 目录中不再扫描嵌套 worktree、虚拟环境等被忽略的目录，避免内存耗尽；同一 worktree 共享一份索引。

---

> **Platform notes**
>
> - The Windows build is **not code-signed** and has **not been tested on Windows**. Issues are welcome.
> - **macOS is not supported.**
>
> **平台说明**
>
> - Windows 构建**未做代码签名**，也**未在 Windows 上测试**，欢迎提交 issue。
> - **不支持 macOS**。
