# 记忆（Memory）设计

状态：已定稿，未实现（2026-10-27 审查修订）。修订：作用域改正为 canonical git root（M1）、写时 + 读时拦截覆盖 Bash 写入（M3）、
提取的请求形态与费用（M2）、提取与压缩的先后（M2.1）、提取位置的存储（M2.2）、合并锁（M5）、双机、决策点（M10）。

设计来源：nyan-legacy N5（旧 roadmap `88ef45bb0:ROADMAP.md` 第 272、296-300 行，Owner Decisions #1、#11）、
`2026-10-26-deferred-context-governance-design-inventory.md` 的"记忆（N5）"一节、Pactile
`compression-decision-chain-v1` S10 与 `03-stage-plan` 阶段 7。偏离 CC 的代码用 `@nyan` 标注。

**定位**：CC 的记忆方案（memdir + MEMORY.md 索引 + 四种类型 + 注入链路）是成熟的基座，本设计只**补充**它缺的
三块（提取、合并、写入安全）和一个机制（候选需确认），不改存储与注入。从 legacy 取的是设计思想，实现优先复用
CC 现成部件（`run_forked_agent`、`dream_task`、secret scanner、`/memory` 选择器）。

## 一、legacy 的设计理念（对齐目标）

1. **两层，结构上不能互相冒充**：
   - **Context Memory**：压缩时提取的 exact facts；摘要永远不是事实来源，检索命中永远不是事实。
   - **Persistent Memory**：跨会话记录，类型固定 `user` / `feedback` / `project` / `reference`。
2. **提取、合并、读取是三个独立操作**；提取失败不回滚已提交的压缩。
3. **写入前安全扫描，宁可拒绝不做局部脱敏**：命中秘密的整段丢弃。
4. **低风险、来源充分的记录按策略自动晋升**；冲突、低置信度、敏感内容留作候选。
   有损压缩、永久 drop、记忆删除是三种单独的显式用户确认，各自留记录（Decision #11）。
5. **记忆只存结构化事实**：prompt、transcript、凭据、绝对路径在写入前拒绝。
6. **跨会话记忆放在会话存储之下，不另起第三个根**（Decision #1）；scope 用 canonical git 根（非 git 目录用 cwd）。

## 二、CC 现状

| 能力 | 现状 | 位置 |
|---|---|---|
| Persistent Memory 存储 | **已有**：`<config>/projects/<sanitize(canonical git root)>/memory/`——先 `find_canonical_git_root`（worktree 解析到主仓库），非 git 目录退回原始 cwd；`MEMORY.md` 索引 + 每条一个带 frontmatter 的 `.md`；类型正好是 `user/feedback/project/reference` | `memdir/paths.rs:89-104`、`utils/git.rs:84-87`、`memdir/memdir.rs:27-104` |
| 注入 | **已有**：系统提示只放行为指引；`MEMORY.md` 正文截断到 ≤200 行 / 25000 字节（`memdir/memdir.rs:11-12`）后作为 `claudeMd` 用户上下文注入首条消息 | `memdir/memdir.rs:416`、`utils/claudemd.rs:320-332,1328-1329`、`context.rs:98-180` |
| 写入 | 只有"主模型自觉调用 Write"；权限层对 memdir 路径的写入**无条件放行**，Write / Edit 与 Bash（`>` 重定向、`sed -i`、`cp` 等）走同一个放行点 | `utils/permissions/filesystem.rs:626-631`（`check_editable_internal_path`），Bash 经 `utils/permissions/path_validation.rs:176` 命中同一放行 |
| 提取 | **无**：`QuerySource::ExtractMemories` 只是枚举值（`constants/query_source.rs:77`）；`stop_hooks.rs:84-88` 注释标为 TODO 接缝 | `query/stop_hooks.rs:140`（`handle_stop_hooks`） |
| 合并（auto-dream） | **无**：`dream_task.rs:10-13` "driver missing"；`autoDreamEnabled` 是死设置；consolidation lock 未移植（`dream_task.rs:19-21,198-201`），但 `/memory` 已在读 `.consolidate-lock` 的 mtime（`components/memory/memory_file_selector.rs:268`）；dream 任务不进后台任务列表（`background_task_items()`，`components/tasks/background_tasks_dialog.rs:37`，见 `dream_task.rs:14-16`） | `tasks/dream_task.rs` |
| 相关记忆注入 | **无生产者**：`Attachment::RelevantMemories` 枚举成员与 `RelevantMemory` 结构、渲染齐全 | `utils/attachments.rs:159`（枚举）、`:552`（结构体）、`utils/messages.rs:3297` |
| 秘密扫描 | 规则齐全（36 条），但规则表与扫描函数都只在 `anthropic_internal` 构建编译 | `services/team_memory_sync/secret_scanner.rs:38`（`SECRET_RULES`）、`:222`（`scan_for_secrets`） |
| 分叉 agent | **已有**：`run_forked_agent`，共享 prompt cache；`CacheSafeParams` 只含 system prompt / user & system context / `tool_use_context` / `fork_context_messages`，**不含 model**——model 取自 `tool_use_context.main_loop_model`；结果带 `total_usage` | `utils/forked_agent.rs:373-379,462-464,518`、`tool.rs:1281` |
| 会话费用 | **未接线**：`cost_tracker` 只累加别处算好的 USD，token → USD 的计算半边未移植，主 API 路径也没有调用 `add_to_total_cost` | `utils/model_cost.rs:4-9`、`cost_tracker.rs:97,113` |
| Context Memory / exact facts | **无** | — |

本机实际使用：`~/.nyan/projects/*/memory/` 下共 6 条记忆，全部是主模型在对话中手写的。

## 三、决定

### M1 存储：沿用 CC 的 memdir（Persistent Memory）

- 不新建存储层。CC 的 memdir 已满足 legacy 的全部存储要求：按项目 scope、类型固定、位于 `$NYAN_CONFIG_DIR/projects/`
  之下（与会话 transcript 同根，符合 Decision #1）。
- **scope = canonical git root**：`get_auto_mem_base` 先 `find_canonical_git_root(original_cwd)`，worktree 会解析到主仓库
  根，非 git 目录才退回原始 cwd（`memdir/paths.rs:89-92`）。所以**同一仓库的所有 worktree 共用一份记忆**——接受：
  记忆记录的是用户偏好、项目背景、外部位置，与 worktree 的内容差异无关（检索索引则按 worktree 分开，见检索 4.8）。
- **两台机器各自一份、不同步**（README 第一节第 7 条）：nyan-pc 与 corp-pc 的 `~/.nyan/projects/*/memory/` 互相独立；
  同一仓库在两机上的 key 也可能不同（路径含用户名）。不做同步、不做合并冲突处理；需要共享的记忆由用户手动复制。
- **"候选"用 memdir 已有的结构表达，不加状态字段**：CC 只注入 `MEMORY.md` 索引（`utils/claudemd.rs:320-331`），
  单条记忆文件不会自动进上下文。所以"已生效" = 文件存在且在索引里；"候选" = 文件存在但不在索引里。确认候选就是把
  它加进索引——用户在 `/memory` 里操作，或手动编辑 `MEMORY.md`，两种方式等价。
- 提取 agent 写的记忆在 frontmatter 里多一行 `source: extract`（`@nyan`），用于 `/memory` 里区分来源；其余字段沿用
  CC 的 `name / description / type`。frontmatter 解析为 `BTreeMap<String, Value>`（`utils/frontmatter_parser.rs:20`），
  多一个键不影响 CC 读取。

### M2 提取：会话末后台提取 agent（填 CC 的 extractMemories 空缺）

- 时机：`handle_stop_hooks`（`query/stop_hooks.rs:140`）中，仅主线程（`QuerySource::Prompt`）、
  非 bare 模式、auto memory 开启时触发；节流为**每 N 轮或会话 token 增长 ≥ T 时一次**
  （默认 N = 8 轮、T = 30k），避免每轮都花钱。另有一个强制触发点：压缩前（M2.1）。
- 执行：`run_forked_agent`，`QuerySource::ExtractMemories`，`skip_transcript = true`，不写进主 transcript。
  提取是 **cache 共享分叉**（README 第一节第 8 条，`ExtractMemories` 在名单内）：
  - `CacheSafeParams`：原样取 `get_last_cache_safe_params()`（`utils/forked_agent.rs:404`，由 stop hook 写入）：
    system prompt、user / system context、`fork_context_messages` = 主线程完整历史，全部与主线程逐字节相同。
  - `prompt_messages`：一条 user 消息：提取指令 + 提取游标（M2.2）+ 当前 `MEMORY.md` 索引。
  - model：主模型——`tool_use_context` 原样沿用，model / profile / effort 都与主线程相同（`tool.rs:1281`）。
  - Agent-Id 头 / 掩码：不带 `X-Claude-Code-Agent-Id`；继承主线程 `ContentReplacementState`，只重放、不新增掩码
    （README 第 8 条三条规则）。
  - 成本：历史全部命中 cache，主要付输出 token。

- 工具：只允许 Read / Glob / Grep 读 memdir、Write / Edit **只限 memdir 目录内**（`can_use_tool` 回调强制，
  目录外一律拒绝；Bash 不给）；`max_turns` 默认 6。
- 提示词：只提取符合四种类型、"从代码或 git 推不出来"的信息（沿用 CC `WHAT_NOT_TO_SAVE_SECTION`），优先更新已有
  记忆而不是新建重复项。
- 提取模型：始终是主模型，不单独配置，也不走 `selection.utility`、不用压缩的摘要模型——换模型或 profile 就丢掉
  cache 共享。
- **费用**：分叉结果的 `total_usage`（`utils/forked_agent.rs:462-464`）计入会话 token 统计（`cost_tracker::add_to_total_tokens`，
  `cost_tracker.rs:113`）。USD 累计在本 port 尚未接线（`utils/model_cost.rs:4-9`：token → USD 的计算半边未移植，主线程
  请求本身也没有计费）；提取请求与主线程走同一条入口，USD 计费随主线程接线一起生效，本模块不单独实现计价。
- 失败只记日志，不打断主会话、不影响压缩；失败时游标不前进（下次重试同一段）。

#### M2.1 提取与压缩的先后（不让待提取的原文先被摘要掉）

顺序事实：提取在每轮末尾的 stop hook 里（`query.rs:2804`），autocompact 在下一次模型请求之前（`query.rs:1072-1084`
→ `auto_compact.rs:241`），手动 `/compact` 走 `commands/compact/compact.rs:77`。节流期间（未满 8 轮 / 30k）若下一次请求前
触发了压缩，游标之后的原文会被摘要替换，提取 agent 之后只能看到摘要。

决定：**压缩前强制提取一次**。

- 在 autocompact 决定要摘要之后、调用 `compact_conversation` / `partial_compact_conversation` 之前，以及手动 `/compact`
  执行摘要之前，若存在游标之后的未提取片段且 `extract` 开启，**同步**运行一次提取（不看节流），完成或失败后再摘要。
- 这次提取与紧接着的摘要请求共享同一前缀（都是压缩前的主线程历史），两者都命中 cache；额外成本是
  一次提取的输出 token + 最多 `max_turns` 次工具往返，换来不丢原文。
- 超时：强制提取有 60 s 上限，超时视为失败、游标不前进，摘要照常进行（不阻塞压缩；该段原文会丢给摘要，日志记
  `extract_skipped_before_compact`）。
- 压缩保留尾部原文（压缩 C7 `keepRecentTokens`）不改变这一点：游标之前、切点之前的内容仍会被摘要。
- 原"摘要里的长期偏好交给提取 agent"（旧 M7）删除：有了压缩前强制提取，偏好在被摘要前已经从原文提取，不需要再从
  摘要里二次提取（摘要不是事实来源，legacy 理念第 1 条）。

#### M2.2 提取游标（"上次提取到哪里"）

- **存储**：主 transcript 里的一个元数据条目 `{"type":"memory-extract-cursor","uuid":<最后一条已提取消息的 uuid>,
  "at":<时间>}`（`@nyan`），由主线程在提取成功后追加（提取分叉本身 `skip_transcript`）。位于 `$NYAN_CONFIG_DIR`
  下的 transcript 中，不写工作树（README 第一节第 6 条）。
- 加进 `METADATA_MARKERS`（`utils/session_storage.rs` 中该常量，压缩切片 A 已在同一处加 `content-replacement`），
  大 transcript resume 时 boundary 之前的游标条目不丢。
- **resume**：从 transcript 中取最后一个游标条目；其 uuid 若已在 compact boundary 之前（被摘要），游标视为位于
  boundary，之后的内容才算未提取。没有游标 = 从会话开头算起。
- 游标只前进、不回退；同一会话多次 resume 互不冲突（只有主线程写）。

### M3 写入安全：写时拦截 + 读时拦截（覆盖所有写入路径）

**检查内容**（同一个函数 `memdir/write_guard.rs::check_memory_content`，任一命中即判违规，不做局部脱敏）：

1. **秘密扫描**：把 CC 的 36 条 secret 规则（`SECRET_RULES` 与 `scan_for_secrets`，`services/team_memory_sync/secret_scanner.rs:38,222`）
   从 `anthropic_internal` 门控中解开（`@nyan`），对 memdir 生效。
2. **legacy 文本启发式**：`authorization:` / `bearer <token>`、`-----BEGIN … PRIVATE KEY-----`、
   `api_key|access_token|password|secret` 赋值。
3. 不做 legacy 的 frontmatter 键白名单与绝对路径拒绝：CC 的记忆是用户也会手写的自由 Markdown，白名单会误拒；
   `reference` 类型本来就记外部位置。legacy 这两条防的是"把 transcript / 凭据塞进记忆"，前者由提取提示词约束，
   后者由上面的秘密扫描兜底。

**问题：写入路径不止 Write / Edit**。权限层对 memdir 路径的写入无条件放行（`utils/permissions/filesystem.rs:626-631`），
Bash 的路径校验经 `utils/permissions/path_validation.rs:176` 命中同一放行点，所以主模型可以用 `echo … > memdir/x.md`、
`sed -i`、`cp`、`python -c` 写入；用户也可能用编辑器直接改。只在 Write / Edit 上拦截挡不住这些路径。

比较过的方案：

| 方案 | 覆盖 | 改动面 | 结论 |
|---|---|---|---|
| (a) 只拦 Write / Edit，接受 Bash 绕过 | 部分 | 最小 | 验收无法兑现"所有路径" |
| (b) Bash 命中 memdir 时降级为 ask / 拒绝（让 `filesystem.rs:626` 的放行只对 Write / Edit 生效） | Bash 中能被路径分析识别的命令 | 中 | Bash 路径分析看不到写入内容，无法做秘密扫描；`python -c`、heredoc 到变量路径等解析不到；`bypassPermissions` 下 ask 会被放行——仍不完整 |
| (c) 读时拦截：内容进入模型上下文之前检查 | 所有写入路径（含 Bash、外部编辑器） | 小：两个读入点 | **采用**，与写时拦截组合 |

**决定：写时拦截（Write / Edit）+ 读时拦截（注入前）**：

- **写时**（即时反馈）：Write / Edit 对 memdir 路径，在权限检查之后、落盘之前对**写入后的完整文件内容**调用
  `check_memory_content`（Edit 检查替换后的全文，不只看 `new_string`）；违规则拒绝整次写入，错误信息给出命中的规则 id，
  不回显命中内容。接入点：`is_auto_mem_path`（`memdir/paths.rs:124`）判定为 memdir 的写入。
- **读时**（兜底，覆盖 Bash 与外部编辑）：记忆内容进入模型上下文只有两个入口，都在注入前调用 `check_memory_content`：
  1. `MEMORY.md` 索引注入：`utils/claudemd.rs` 中 `ClaudeMdKind::AutoMem` 的读入处（`:1327` 解析后、`:1328-1329` 截断前）；
  2. M6 `RelevantMemories` 的生产者（检索落地后），对每个候选文件。
- 读时命中违规：该文件**不注入**（索引违规则整个索引本轮不注入，退化为"无记忆"），记 `memory_guard_blocked`
  日志，并在 `/memory` 里把该文件标为"已隔离：命中秘密规则 <id>"。**不删除、不改写文件**（删除需用户确认，Decision #11）。
- 不拦截 Read / Grep 等工具对 memdir 的直接读取：这与读取工作区里任何含秘密的文件同性质，不属于记忆注入。
- (b) 不做：它增加审批打断，却仍覆盖不全；(c) 已经让违规内容进不了上下文。

接入点汇总：`tools/file_write_tool`、`tools/file_edit_tool`（写时），`utils/claudemd.rs`（读时），M6 生产者（读时，检索切片 I）。

### M4 晋升：自动 + 候选（Decision #11）

- 提取 agent 写记忆文件后，**自己决定是否加进索引**：低风险的直接加（生效）；以下情况只写文件、不加索引（候选），
  并在本次提取的结果里列出：
  1. 与已有记忆冲突（应当更新旧条目而它拿不准）；
  2. 类型为 `user`（关于用户本人的判断，敏感度高）；
  3. 它自己没把握。
- 第 2 条由工具约束兜底：提取 agent 修改 `MEMORY.md` 时，若新增的索引行指向 `type: user` 的文件，写入被拒
  （`can_use_tool` 回调检查）。
- 主模型在对话中按用户明确要求写的记忆照 CC 原样直接生效（用户已表态）。

### M5 合并（consolidation，填 CC 的 auto-dream 空缺）

- 沿用 CC 上游 auto-dream 的形态，只补缺失的驱动：`tasks/dream_task.rs` 已有任务状态机、`files_touched`、
  `prior_mtime`（`:81`）、kill 入口（`kill_dream_task`，`:202`）。`autoDreamEnabled` 从死设置变成真开关（`@nyan`）。
- 执行：`run_forked_agent`，`QuerySource::AutoDream`。合并不依赖当前对话内容，但为了共享 cache 与提取一样是 cache
  共享分叉（README 第一节第 8 条名单内：主模型、不带 Agent-Id、只重放掩码、`CacheSafeParams` 原样复用），
  `prompt_messages` 只含合并指令与 memdir 清单。会话里还没有 cache-safe 参数（首轮之前）时不触发。
- 触发：会话空闲（一轮结束、等待用户输入）时检查阈值——距上次合并 ≥ `consolidateMinHours`（默认 24）且 memdir 中
  mtime 比锁文件新的记忆文件 ≥ `consolidateMinNewFiles`（默认 5）。手动：`/memory consolidate`（不看阈值）。
- 内容：去重、合并相近条目、更新过时的 `project` 记忆、重建 `MEMORY.md` 索引。
- **删除需要确认**（Decision #11）：合并 agent 不能删除文件（`can_use_tool` 拒绝对 memdir 的删除；Bash 不给）。被合并掉
  或过时的条目，它从索引里移除、文件留着，并在结果里列为"建议删除"；不在索引里的文件本来就不会被注入，用户在
  `/memory` 里确认后由 nyan 删除。不需要回收站目录。
- 工具约束同 M2（只限 memdir）；合并写入同样经过 M3 写时检查。

#### M5.1 合并锁 `.consolidate-lock`（移植 CC `consolidationLock`）

`/memory` 已经按 `<memdir>/.consolidate-lock` 的 mtime 显示"last ran …"（`components/memory/memory_file_selector.rs:268`），
锁服务本身未移植（`dream_task.rs:19-21,198-201`）。新 `services/auto_dream/lock.rs`：

- **语义**：锁文件的 **mtime = 上次合并开始时间**；文件内容 = 持锁进程的 `pid` 与开始时间。
- **获取**：触发合并前 `O_CREAT` 打开并检查内容——若内容中的 pid 仍存活且开始时间在 1 小时内，视为他人持锁，本次不
  合并；否则（不存在 / 进程已死 / 超过 1 小时，视为崩溃遗留）写入自己的 pid，并把 mtime 设为现在。获取前记下原 mtime
  作为 `prior_mtime`，传给 `register_dream_task`。
- **完成**：合并成功后清空 pid 内容，mtime 保留为本次开始时间（即"上次合并时间"）。
- **回滚**：合并失败或被 kill（`kill_dream_task`）时，把 mtime 恢复为 `prior_mtime` 并清空 pid——让下一次空闲能重试，
  而不是被当作"刚合并过"推迟 24 小时。这就是 `dream_task.rs:198-201` 注释里缺的 `rollbackConsolidationLock`。
- **崩溃**：进程在合并中途崩溃时锁里留着死 pid，下一次获取时按上面的规则回收；mtime 已被设为开始时间，最坏情况是
  推迟一个阈值周期，不会重复并发合并。
- **双机**：锁在各机自己的 memdir 下（README 第一节第 7 条），两机互不影响。
- 后台任务列表接线：`background_task_items()`（`components/tasks/background_tasks_dialog.rs:37`）目前只构建 Shell 与
  LocalAgent 项（`dream_task.rs:14-16`），补一个 Dream 分支，让运行中的合并出现在 footer pill 与 Shift+Down 列表，
  并能用 `x` kill（`dream_task.rs:17-18` 所述未接的 dialog 分支）。

### M6 相关记忆注入（RelevantMemories）

- MEMORY.md 正文已全量注入（≤200 行），记忆少时这已足够。只有当记忆条数超过索引上限、或单条正文较长时，
  才需要"按当前问题挑选相关记忆的全文"注入。
- 本模块**只定义接缝，不实现选择算法**：选择依赖检索模块（N6）。检索落地后，在每次用户输入时按问题检索
  memdir，命中项以 `Attachment::RelevantMemories` 注入（CC 渲染已就绪）。
- 在检索落地前不注入任何东西（不用关键词匹配凑合）。

### M7 Context Memory（exact facts）

- legacy 的设计：压缩时把"必须原样保留的事实"（精确的路径、命令、错误信息、用户约束）提取成不可变的事实文档，
  摘要只能引用，模型可通过 `memory_read` 回读。
- 本 fork 的处理：**不做独立存储与 `memory_read` 工具**。理由：
  1. 压缩已经保留了尾部原文（compaction C7 `keepRecentTokens`），且 transcript 里的原文永不删除；
  2. legacy 摘要格式里已有 `Constraints & Preferences` 与 `Critical Context` 两节，要求"保留精确路径、
     函数名、错误信息"；
  3. artifact → receipt → manifest CAS 的整套机制服务于多写者、跨进程的 Pactile 场景，单机 CLI 不需要。
- Context Memory 的跨会话部分由 Persistent Memory 承接，具体机制是**压缩前强制提取**（M2.1）：原文被摘要之前，
  提取 agent 已经从原文中把长期偏好与约束写成记忆。不从摘要里二次提取（摘要不是事实来源）。
- 这是与 legacy 的明确偏离，见"四、明确不做"。

### M8 `/memory` 命令

在 CC 现有的选择器（`components/memory/memory_file_selector.rs`）上增加：

- 列出 memdir 中**不在索引里的记忆文件**（候选，以及合并后建议删除的旧条目），支持"加入索引"或"删除"；
- 子命令 `/memory consolidate` 手动触发合并；
- 显示上次提取、上次合并的时间与结果。

### M9 开关

挂在 `settings.nyan.memory` 下：

| 设置 | 默认 | 作用 |
|---|---|---|
| `extract` | `true` | 后台提取 |
| `extractEveryTurns` | `8` | 提取节流：轮数 |
| `extractEveryTokens` | `30000` | 提取节流：token 增长 |
| `extractBeforeCompact` | `true` | 压缩前强制提取（M2.1） |
| `extractBeforeCompactTimeoutMs` | `60000` | 压缩前强制提取的上限，超时则跳过、摘要照常 |
| `extractMaxTurns` | `6` | 提取 agent 的最大轮数 |
| `consolidate` | `"auto"` | `"auto"` / `"manual"` / `"off"` |
| `consolidateMinHours` | `24` | 自动合并：距上次合并的最小间隔（小时） |
| `consolidateMinNewFiles` | `5` | 自动合并：比锁文件新的记忆文件数下限 |
| `secretScan` | `true` | 写时与读时检查（M3）；关闭后两处都不检查 |

CC 原有的 `autoMemoryEnabled = false` 时，提取与合并一并关闭。

### M10 决策点（Decision Provider 接缝）

本模块中可能交给快速决策模型的判断点。**是否接入、接口与评测由 [decision-provider.md](decision-provider.md) 决定；
未接入或 Decision Provider 不可用时一律用下表的确定性规则（默认）**。本模块不定义接口。

| 决策点 | 位置 | 确定性规则（默认 / 回退） | Decision Provider（decision-provider.md 4.3） |
|---|---|---|---|
| 这一轮是否该**提前**提取 | M2 节流 | 每 8 轮或 token 增长 ≥ 30k | `memory.extract_now`（active）：判"有可记内容"时提前触发，每 3 轮最多 1 次 |
| 节流到点时是否**跳过**提取 | M2 节流 | 按规则提取 | `memory.extract_skip`（shadow）：连续跳过 ≥ 3 次强制一次；压缩前强制提取（M2.1）不可跳过 |
| 新记忆的类型 | M2、M4 | 提取 agent 自判 | `memory.type`（active）：与提取 agent 不一致时转为候选；`type: user` 仍由硬规则判为候选 |
| 新记忆是否与已有条目重复 | M2、M4 | 提取 agent 自判（提示词要求优先更新） | `memory.duplicate`（active）：判重复时转为候选并提示，不自动合并、不删除 |
| consolidate 是否该跑 | M5 | ≥ 24 h 且 ≥ 5 个新文件；手动命令不看阈值 | `memory.consolidate`（shadow）：可提前到 ≥ 12 h 且 ≥ 3 个、或推迟到 ≤ 72 h；手动命令不经 Decision Provider |
| 读时检查是否误报 | M3 | 命中即隔离 | 不接入（安全拦截必须确定性） |

## 四、明确不做

| 项 | 理由 |
|---|---|
| Context Memory 独立存储、`memory_read` 工具、artifact→receipt→manifest CAS | 见 M7：压缩保留尾部原文 + transcript 永存 + 摘要格式已覆盖；CAS 链服务多写者场景 |
| `scope-v1:` scopeKey、workspace/worktree 逻辑 id | CC 的 canonical git root 已是项目 scope，worktree 共享记忆是接受的（M1） |
| 两机记忆同步 / 合并冲突处理 | README 第一节第 7 条：各机一份、不同步（M1） |
| Bash 写 memdir 的写时拦截 | 路径分析看不到内容、覆盖不全；由读时拦截兜底（M3 方案比较） |
| Kernel command / approval chain / revoke receipt、状态字段、回收站 | 无 Kernel；"是否在 MEMORY.md 索引里"本身就是生效状态，确认 = 加入索引，删除需用户在 `/memory` 执行 |
| 绝对路径拒绝 | 见 M3"检查内容"第 3 点 |
| 关键词匹配凑合的相关记忆选择 | 等检索模块，见 M6 |
| team memory | CC 默认关闭（`feature_flags.rs:512`），与本 fork 目标无关，保持原样 |

## 五、已确认的取舍（2026-10-26）

1. M7：不做 Context Memory 独立层，由"保留尾部原文 + transcript + 摘要格式 + 压缩前强制提取"承接。
2. M2 节流默认 8 轮 / 30k token。
3. M4 `user` 类型默认为候选，需确认后生效。
4. M5 自动合并阈值 24 小时 + 5 条新增（配置项 `consolidateMinHours` / `consolidateMinNewFiles`）。
5. M1 同仓库 worktree 共享记忆；两台机器记忆不同步（2026-10-27）。
6. M3 写时 + 读时拦截，Bash 写入由读时兜底（2026-10-27）。

## 六、cache 约束（与压缩模块一致）

- 提取与合并都是 **cache 共享分叉**（README 第一节第 8 条，`ExtractMemories` / `AutoDream` 在名单内）：用主模型、
  `CacheSafeParams` 原样复用、不带 `X-Claude-Code-Agent-Id`、只重放掩码不新增，历史部分全部命中主会话 cache，
  只付输出 token。判别统一用 `is_cache_sharing_fork`，本模块不另写名单。
- 压缩前强制提取（M2.1）与紧接着的摘要请求共享同一前缀，两者都命中 cache。
- 提取与合并的请求不得改变主会话后续请求的前缀（不写主 transcript 的消息、不改主会话消息；提取游标是元数据条目，
  不进模型上下文），保证主会话 cache 不受影响。

## 七、实现切片

| 切片 | 内容 | 主要写集 | 依赖 |
|---|---|---|---|
| A | 写入安全：解开 secret 规则门控、启发式、`check_memory_content`；写时接入 Write / Edit（完整文件内容）；读时接入 `MEMORY.md` 注入；`/memory` 隔离标记的数据来源 | `services/team_memory_sync/secret_scanner.rs`、新 `memdir/write_guard.rs`、`tools/file_write_tool`、`tools/file_edit_tool`、`utils/claudemd.rs` | — |
| B | 候选机制：列出不在索引里的记忆文件；提取 agent 修改索引时的 `type: user` 检查 | `memdir/memdir.rs` | — |
| C | 提取 agent：节流、cache 共享分叉、工具约束、提示词、费用（`total_usage` → `cost_tracker`）、提取游标条目与 `METADATA_MARKERS`、压缩前强制提取、`settings.nyan.memory` | 新 `services/extract_memories/`、`query/stop_hooks.rs`、`services/compact/auto_compact.rs`、`commands/compact/compact.rs`、`utils/session_storage.rs`（游标条目类型与 marker）、`utils/settings/types.rs` | A、B、公共（`is_cache_sharing_fork`，README 批次 0） |
| D | 合并 agent：dream 驱动、空闲触发与阈值、`.consolidate-lock` 获取 / 完成 / 回滚 / 崩溃回收、禁止删除、建议删除清单、后台任务列表的 Dream 分支 | 新 `services/auto_dream/`（含 `lock.rs`）、`tasks/dream_task.rs`、`components/tasks/background_tasks_dialog.rs` | A、B、公共（`is_cache_sharing_fork`） |
| E | `/memory` UI：不在索引里的文件（加入 / 删除）、隔离文件标记、手动合并、上次提取 / 合并的时间与结果 | `components/memory/memory_file_selector.rs`、`commands/memory.rs` | B、D |

A、B 可并行；C、D 在 A、B 之后可并行；E 最后。M6 不在本模块实现（读时检查接入 M6 生产者属于检索切片 I）。
切片 C 与压缩切片 A 都改 `utils/session_storage.rs` 的 `METADATA_MARKERS`（各加一项），合并时注意冲突；与压缩切片
E 都涉及 `auto_compact.rs`（C 只在摘要调用前加一个钩子）。

## 八、验收

1. Write / Edit 写入含 API key / 私钥 / bearer token 的记忆被整次拒绝，主模型与提取 / 合并 agent 都一样；Edit 按替换后
   的全文检查。
2. 用 Bash（`echo … > <memdir>/x.md`）或外部编辑器写入含秘密的记忆文件后：该文件不被注入（若写的是 `MEMORY.md`，
   本轮不注入索引），日志有 `memory_guard_blocked`，`/memory` 中标为已隔离；文件本身未被删除或改写。
3. 提取 agent 无法写 memdir 以外的任何路径，也没有 Bash。
4. 提取只在节流条件满足时触发，或在压缩前强制触发；失败不影响主会话与压缩，失败时游标不前进。
5. 压缩前存在未提取片段时，摘要请求发出之前已完成（或超时跳过）一次提取；超时有 `extract_skipped_before_compact` 日志。
6. 提取不写主 transcript 的消息；提取游标条目在 resume 后可读（含 transcript > 5 MB 的情况）；提取的 `total_usage`
   计入会话 token 统计。
7. 提取请求与主线程最后一次请求用同一模型、前缀逐字节相同，且不带 `X-Claude-Code-Agent-Id`。
8. 候选记忆不出现在 MEMORY.md 索引、不注入；在 `/memory` 里加入索引后才生效。
9. 合并 agent 无法删除任何文件；建议删除的文件经用户确认后才删除。
10. 合并被 kill 或失败后锁文件 mtime 恢复为合并前的值；持锁进程崩溃后，下一次触发能回收锁；运行中的合并出现在
    后台任务列表。
11. `autoMemoryEnabled = false` 时提取与合并都不运行。
12. 现有记忆（无 `source` 字段）照常加载、照常生效；同一仓库的不同 worktree 读到同一份记忆。
