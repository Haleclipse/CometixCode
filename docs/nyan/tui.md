# TUI 呈现重构：内置信息行、todo 面板、agent 面板

状态：已实现（2026-11-13 定稿；切片 A–E 与 E2 已合入 master，真实终端验收通过）。后续：agent 完整 transcript
视图（Enter 切换并可继续对话）、模型生成的进度摘要、通用模型短名，均另开需求。继续使用 iocraft / CometixTUI 与现有主题，不引入新配色、不依赖 Nerd Font。

## 一、目标与范围

1. 删除 statusLine（组件、命令、hook、settings 字段、通知），改为内置信息行。
2. 页脚两行：第 1 行信息行（环境 + 会话指标），第 2 行保持模式指示与提示。
3. todo 面板：任务列表、每条状态、整体进度；Ctrl+T 展开 / 收起。
4. agent 面板：子 agent、后台 shell、teammate 的运行情况；有任务时自动出现。

**明确不做**

- 不显示 provider、费用、未提交改动状态。
- 不显示上下文百分比（绝对量已足够，用户自己会算）。
- 不做模型短名规则：`render_model_name`（`utils/model/model.rs:139`）只认 Claude 系；非 Claude 模型原样显示。
  通用短名是**独立需求**，不能只对 Claude 优化，另行设计。
- 不在页脚显示会话累计缓存命中率（留给 `/cost`、`/stats`）。
- 不改生产渲染模式（仍是 inline + 原生 scrollback，`FullscreenLayout` 生产未挂载）。
- 不改 spinner 本身（动词、计时、`↓ N tokens` 估算保持现状）。

## 二、整体布局

inline 模式下，消息进 scrollback；只有底部"活动区"重绘。活动区从上到下：

```
✻ 重构页脚… (1m 12s · esc to interrupt)                         ← spinner（不改）
  ⎿  ✔ 调研现状  +2                                              ← todo 面板
     ◼ 重构页脚布局
     ◻ 删除 statusLine 机制
     ◻ 窄屏降级                                             3/7
╭──────────────────────────────────────────────────────────────╮
│ >                                                            │ ← 输入框
╰──────────────────────────────────────────────────────────────╯
  ⠋ explore   Read src/components/agent_panel.rs  14 tools · 38k · 2m10s ← agent 面板
  ⠋ general   WebSearch "opencode subagent tui"    6 tools · 12k · 1m02s
  ⠋ $ cargo test --lib                                             45s
  nyan  master ◇            Opus 4.6 high · 84k/200k · cache 91% · 52 t/s   ← 信息行
  ⏵⏵ accept edits on · ↓ agents                         ? for shortcuts    ← 模式行
```

- todo 面板在输入框上方：有 spinner 时挂在 spinner 下方，用 `⎿` 引出；空闲时为带标题的独立块（见 4.2）。
- agent 面板在输入框下方、信息行上方（Footer 内），与 Claude Code 一致（"a panel below the prompt input"），
  所以 `↓` 进入、首行 `↑` 返回的方向是自然的。E 曾把它放在输入框上方，方向相反，已在 E2 改正。

## 三、页脚

### 3.1 信息行（第 1 行，替代 statusLine 的位置）

```
  {目录} {分支} {◇}                         {模型} {思考等级} · {上下文} · cache {率} · {tps}
```

左段环境、右段指标，中间空白填充；段间分隔 ` · `。整行默认 `inactive`，只有异常段变色。

| 段 | 内容 | 颜色 | 不显示的情况 |
|---|---|---|---|
| 目录 | cwd 的 basename；在仓库子目录时为 `<repo>/<子路径>`，过长从左截断 | `text` | — |
| 分支 | 分支名；detached 时 `@<short-sha>`；过长中间截断 `feat/…footer` | `inactive` | 不在 git 仓库 |
| worktree | 只显示 `◇`，不显示名称 | `claude_blue` | 不在 linked worktree |
| 模型 | `render_model_name(model)` | `inactive` | — |
| 思考等级 | `get_displayed_effort_level`（`utils/effort.rs:134`） | `inactive` | effort 为 Off |
| 上下文 | `84k/200k`（见 3.3） | 两级变色（见 3.3） | 首个请求前 |
| cache | `cache 91%`（见 5.2） | 失效时 `warning` | 本会话从未报告过 cache 字段 |
| tps | `52 t/s`（见 5.1） | 生成中 `text`，否则 `inactive` | 本会话还没有样本 |

**worktree 判定**：`git rev-parse --git-dir` 与 `--git-common-dir` 不同即为 linked worktree，覆盖用户手建的
worktree，不只 nyan 自己创建的会话（`get_current_worktree_session`，`utils/worktree.rs:46`）。

**git 信息刷新**：`get_branch()`（`utils/git.rs:162`）会起子进程，不能在 render 里调。分支、sha、worktree 用一次
`git rev-parse` 取齐，后台执行，结果写入 AppState；触发时机：启动、每轮结束、Bash 工具完成、cwd 变化。

### 3.2 窄屏降级

按优先级逐项省略，直到放得下（不换行）：

1. 省 cache
2. 省目录
3. 省 tps
4. 省思考等级
5. 分支中间截断
6. 模型名尾部截断 `…`

上下文与模型名（截断后）始终保留。参考效果：

```
≥100  nyan  master ◇                 Opus 4.6 high · 84k/200k · cache 91% · 52 t/s
 80   nyan  master ◇                      Opus 4.6 high · 84k/200k · cache 91%
 60   master ◇                                   Opus 4.6 high · 84k/200k · 52t/s
<50   master ◇                                               Opus 4.6 · 84k/200k
```

（实际按逐项测宽，不按固定断点；窄宽下 `52 t/s` 去空格为 `52t/s`。）

### 3.3 上下文段

- 分子：与 auto-compact 判定同一个数（最近一次主线程请求的 `input + cache_creation + cache_read`，
  `utils/context.rs:86`）。
- 分母：模型真实窗口 `get_context_window_for_model`（`utils/context.rs:153`），1M 模型显示 `84k/1M`。
- 格式：`<1000` 原样；`<1M` 为 `84k`（取整）；`≥1M` 为 `1.2M`。
- **两级变色**：基准 `threshold` = auto-compact 阈值（关闭 auto-compact 时为有效窗口），沿用
  `calculate_token_warning_state_with_config`（`services/compact/auto_compact.rs:132`）。
  - `token_usage ≥ threshold − 40k` → `warning`
  - `token_usage ≥ threshold − 20k` → `error`
  - 实现：`WARNING_THRESHOLD_BUFFER_TOKENS` 由 20k 改为 40k（`auto_compact.rs:17`）；现在两个缓冲都是 20k，
    两级会同时触发。

### 3.4 模式行（第 2 行）

- 左侧保持现状：exit hint、vim 模式、权限模式 + `(shift+tab to cycle)`、导航提示
  （`prompt_input_footer_left_side.rs`）。
- 去掉 tasks / teammates pill（由 agent 面板取代；仅在高度降级时回来，见第六节）。
- 右侧保留 IDE、通知、auth、Debug、自动更新、内存、沙箱（`notifications.rs:444-491`），以下两项收敛：
  - **TokenWarning**：不再显示长句 `Context low (N% remaining) · Run /compact…`，改为短提示，颜色与信息行
    上下文段一致：auto-compact 开 → `auto-compact soon`；关 → `/compact to free context`。
  - **verbose 的 `N tokens`**（`notifications.rs:417-421`）：删除，信息行已覆盖。

## 四、todo 面板

### 4.1 数据与状态

- 数据：当前任务列表（TaskCreate / TaskUpdate 写盘，`utils/tasks.rs`）。v1 `TodoWrite` 只在非交互模式启用，
  没有 TUI，面板不接。`_internal` 任务不显示（同 TaskList 工具）。
- 刷新：任务在磁盘上、无订阅。TaskCreate / TaskUpdate 写盘后递增 `AppState.task_list_revision`；面板只在
  版本号或任务列表 id 变化（`/clear`、`/resume` 换 session id）时重读，不逐帧读盘。
- 实现在 `components/task_list_v2.rs`（`TodoPanel` 连接状态，`TodoPanelView` 只绘制，布局为纯函数）。
- spinner 不再从任务列表取 `Next: …` 提示（面板已列出），该位置回到 Tip。
- 排序：已完成（折叠）→ 进行中 → 待办，组内按 id。

| 状态 | 图标 | 样式 |
|---|---|---|
| 进行中 | `◼` | `claude`，加粗 |
| 待办 | `◻` | `inactive` |
| 待办且被阻塞 | `◻` | `inactive`，尾部 ` · blocked` |
| 已完成 | `✔` | `success` 图标，标题 `inactive` + 删除线 |

- 已完成项折叠成一行 `✔ <最后完成的一项>  +N`。
- 最多 6 行条目；超出的待办折叠为 `… +N pending`。
- 右侧进度 `完成数/总数`；有 owner 时标题后 `@owner`（`inactive`）。

### 4.2 形态

**展开（spinner 期间）**：见第二节。

**展开（空闲）**：

```
  Tasks 3/7                                           ctrl+t to collapse
  ✔ 调研现状  +2
  ◼ 重构页脚布局
  ◻ 删除 statusLine 机制
```

**收起**（只剩一行，显示当前进行中的条目；没有进行中时用第一个待办 `◻`）：

```
  ⎿  ◼ 重构页脚布局 · 3/7                                         ctrl+t
```

**全部完成**：一行 `✔ 7/7 done`，保留到下一次用户提交，然后整块消失（提交时记下任务签名
`AppState.todo_cleared_signature`；任务有任何 id / status 变化即重新出现）。

### 4.3 展开规则

- TaskCreate / TaskUpdate 时自动展开（`AppStoreRef::auto_expand_tasks_panel`，`tool.rs`）。
- Ctrl+T 只在展开（`expanded_view = Tasks`）/ 收起（`None`）间切换，不再进入 `Teammates`（agent 面板取代，
  见第六节）。**手动收起后，本会话内不再自动展开**（`todo_auto_expand_suppressed`），直到再按 Ctrl+T。
- 状态只在会话内有效，不持久化（原先写 `showExpandedTodos` / `showSpinnerTree`，但启动从不读回，已删）。
- 列表为空时不显示任何行。

## 五、指标口径

调研依据（源码事实）：opencode `packages/util/src/session-turn.ts:37-56`、new-api `pkg/perf_metrics/metrics.go`、
ccstatusline `src/widgets/shared/cache-metrics.ts`、gemini-cli `computeStats.ts`、CLIProxyAPI
`sdk/cliproxy/usage/accounting.go`。codex、crush、gemini-cli 页脚均不显示 tps。

### 5.1 tps：解码速率

```
tps = Σ output_tokens ÷ Σ (流结束 − 首个内容 delta)
```

- **样本**：主线程本轮（自用户提交起）的每个流式 API 请求。子 agent、cache 共享分叉（压缩、记忆、保活等）不计入。
- **分子**：请求最终 usage 的 `output_tokens`，含思考 token（Anthropic 的 output 本身包含 thinking；OpenAI 系
  `completion_tokens` 含 reasoning）。
- **分母**：首个内容 delta（text / thinking / tool input）到 `message_stop`。扣除 TTFT 与预填充，工具执行时间
  天然不在内。注意现有 `first_ttft_ms`（`services/api/claude.rs:4210`）记的是首个任意流事件，含 `message_start`，
  需要另记首个内容 delta 的时间戳。
- **聚合**：本轮内先求和再相除（同 opencode / new-api），不是逐请求平均。
- **刷新**：每个请求结束时用准确 usage 更新一次；流式过程中**不估算**（output 通常末尾才给；Claude 4 的 thinking 为
  摘要，`字符数/4` 会严重偏低）。
- **排除**：非流式请求、被中断且无最终 usage 的请求、分母 < 200 ms 的请求（避免极短请求产生尖峰）。
- **显示**：本轮生成中且已有本轮样本 → `text`；空闲 → 保留上一轮值，`inactive`；`<10` 保留一位小数。

### 5.2 cache：读缓存占全部输入的比例

```
cache = cache_read ÷ (uncached_input + cache_read + cache_write)
```

- 直接用已有的 `Usage::cache_hit_rate()`（`services/provider/usage.rs:44`）；各协议已在 provider 适配层归一为三项
  独立（OpenAI / Kimi 的 cached 从 prompt 中拆出，`usage.rs:92` 起）。实现第一步核对：TUI 读到的消息级
  `TokenUsage`（`types/message.rs:1261-1264`）与 `ir::Usage` 是同一口径。
- **范围**：最近一次主线程请求，与上下文段一致。
- **显示门槛**：本会话至少一次请求报告了 `cache_read > 0` 或 `cache_write > 0` 才显示（不支持缓存的 provider 不显示
  恒为 0% 的误导值）。
- **失效告警**：`Usage::detect_cache_miss`（`usage.rs:59`，目前无调用方）返回 `Some` 时该段变 `warning`，直到下一次
  正常请求。以下情况不告警：本会话首个请求、压缩后首个请求、切换模型后首个请求。

## 六、agent 面板

### 6.1 内容

- 来源：后台任务——异步 agent（local agent 注册表）、in-process teammate、后台 shell、dream（`AppState.tasks`）。
  remote session 运行时尚未移植，暂不接入。前台同步子 agent 与前台 shell 已在消息流中显示，不重复进面板。
- 实现在 `components/agent_panel.rs`（投影、生命周期、行数规划 `plan_panel`、布局为纯函数），挂在 Footer 内
  信息行之上。`AgentPanel` 把显示的行写入 `AppState.agent_panel_items` / `agent_panel_row_count`，页脚导航、
  Enter / `x` 路由与高度预算都读这一份。
- 有可见行或完成提示未过期时 1 秒一次刷新（spinner 帧与耗时同步走），否则停表，不做高频动画。
- 行格式（参照调研：Claude Code、opencode、gemini-cli、kimi-cli、crush）：

```
  ⠋ explore   Read src/components/agent_panel.rs  14 tools · 38k · 2m10s
  ⠋ $ cargo test --lib                                             45s
```

  - 行首标记：运行中为 spinner 帧，完成 `✔`（`success`），失败 / 停止 `✘`（`error`）。名字用 `agent_*` 色。
  - 活动列为**最近一次工具调用**（`activity_description`，无则工具名），不做模型生成的进度摘要（CC 那样每 ~30 s
    额外一次请求，有成本；以后可单独加）。shell 为命令，dream 为阶段。统计 `inactive`、右对齐。
- 生命周期（同 CC）：成功完成的行**立即移除**，模式行显示 30 秒 `/tasks to see agents`；失败 / 停止的行保留
  30 秒。首次看到即为终态的任务不显示。"见过运行中"的记录全进程一份：PromptInput 按 generation 重挂，
  组件内状态会丢。
- 窄屏：先省右侧统计，再截断活动描述。

### 6.2 交互

- 复用页脚选择机制：面板可见时 `FooterItem::AgentPanel` 是第一个 footer 项（取代 Tasks / Teams），
  `AppState.agent_panel_selected` 记面板内行号。
- 未聚焦时模式行提示 `↓ agents`（面板可见才有）。输入框为空时 `↓` 聚焦面板第一行；`↑` / `↓` 移动选中，
  末行再 `↓` 到下一个 footer 项；`Esc` 或在首行按 `↑` 回到输入框。行数变少时行号收拢，行全部消失时清除选择。
- 聚焦时：选中行前缀 `▶`（`claude`）+ `selection_bg`，其下展开最近 3 次工具调用（`⎿` 引出，`inactive`；
  shell / dream 无历史不展开）；面板末行显示键位提示 `↑↓ select · enter details · x stop · esc back`（键名读实际绑定）。
- `Enter`：teammate 行打开 `TeamsDialog`；其他行打开 `BackgroundTasksDialog` 并以 `initial_detail_task_id` 直达详情。
  切换到 agent 完整 transcript 并继续对话（CC / opencode 的做法）另开切片。
- `x`（Footer 上下文 `footer:stopSelected`）：停止选中任务，与对话框共用 `stop_background_task`
  （dream / teammate / local agent / shell 分派）；已结束的行无效果。
- 取代：spinner 下的 `TeammateSpinnerTree`（已删）与 `ExpandedView::Teammates`（已删）；面板可见时页脚不渲染
  tasks / teammates pill。

## 七、高度降级

inline 模式下活动区过高会导致重绘闪烁，按终端行数限高：

| 终端行数 | todo | agent |
|---|---|---|
| ≥ 24 | 最多 6 行条目 | 最多 4 条任务行，面板总高（含展开与键位提示）≤ 8 |
| 16–23 | 最多 3 行条目 | 最多 2 条任务行，不展开，总高 ≤ 3 |
| < 16 | 强制一行（收起形态） | 不显示面板，模式行恢复 `● N agents` pill（保留 `↓ to manage` 导航） |

## 八、statusLine 删除范围

整体删除，不保留兼容：

- `components/status_line.rs`、`commands/statusline.rs`（及 `commands/mod.rs:772` 注册）、
  `services/hooks/statusline.rs`、`types/status_line.rs`、`hooks/notifs/statusline.rs`、`main.rs:468` 启动通知。
- settings：`StatusLineSettings` 与 `status_line` 字段、schema（`utils/settings/types.rs:91`、`:227-232`、`:899`）。
- AppState：`status_line_text`（`state/app_state_store.rs:623`）。
- 页脚挂载：`prompt_input_footer.rs:279-316`。
- 其余引用（hooks 配置菜单、trust / managed settings 对话框、tips、hook events、managed env、sandbox 等，
  `rg -l 'status_?line|statusLine'` 共约 35 个文件）在切片第一步逐个清理；测试中借用 `status_line_text`
  做计数器的（`state/store.rs:1094`、`sandbox_adapter.rs:2712`）改用其他字段。
- 本机 `~/.nyan/settings*.json` 未配置 `statusLine`，删除字段不影响现有配置。

## 九、实施切片

| 切片 | 内容 | 主要写集 | 依赖 |
|---|---|---|---|
| A | 指标采集：首个内容 delta 时间戳、本轮 tps 聚合、cache 显示门槛与失效判定、git / worktree 后台刷新，写入 AppState | `services/api/claude.rs`、`state/`、`utils/git.rs` | — |
| B | 删除 statusLine 全链路 | 见第八节 | — |
| C | 信息行组件 + 窄屏降级；模式行收敛（TokenWarning 短提示、删 verbose tokens、两级阈值） | `prompt_input/prompt_input_footer*.rs`、`notifications.rs`、`token_warning.rs`、`auto_compact.rs` | A、B |
| D | todo 面板挂载与形态；Ctrl+T 新语义 | `task_list_v2.rs`、`use_global_keybindings.rs`、`on_change_app_state.rs`、`repl.rs` | C |
| E | agent 面板、聚焦交互、高度降级；移除 spinner tree 与 pill | 新组件、`spinner/`、`prompt_input_footer_left_side.rs`、`repl.rs` | D（共用 `repl.rs` 活动区） |

A 与 B 写集不重叠，可并行。

**验证**：每个组件用 iocraft canvas 渲染测试（参照 `status_line.rs:656` 的写法），宽度 120 / 80 / 60 / 40 各一组，
断言文本与关键段颜色；tps / cache 口径用构造的 usage 序列做单测（含中断、非流式、极短请求、压缩后首请求）；
高度降级按 30 / 20 / 12 行渲染。收尾在真实终端手测一轮宽窄与 resize。
