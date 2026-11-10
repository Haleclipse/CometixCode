# 压缩（Compaction）设计

状态：已定稿，未实现（2026-10-27 审查修订）。修订补上了 cache 共享分叉只重放不新增掩码、命令路径只投影、保活与
thinking 的兼容判定、Read 去重改为反查替换状态、resume 漏洞的准确成因、摘要校验落点、time-based 移除的连带面，
并删除了"resume 后 usage 不可信"的规则，新增 C14 决策点。cache 保活（C12）与 ContentReplacementState 默认开启已确认；
C13 已查清（统计口径错误 + 网关侧断崖 + 子代理缺 Agent-Id，新增切片 G）。

本文是 nyan 的压缩能力二次开发设计。设计来源是
nyan-legacy 的 N4 定稿（`.agents/notes/proposed/architecture/2026-10-22-n4-slices-and-admission-design.md`）
和 durable spec §8.7，按 CC 的现有结构落地。所有有意偏离现有行为的代码位置用 `@nyan` 注释标注。

**定位**：CC 的压缩方案是成熟的基座，本设计是在它之上**补充与轻改**，不是替换。从 legacy 取的是设计思想
（先清后摘、清而不删、失败不伪装、cache 不因时间失效），实现优先复用 CC 现有机制；legacy 的具体实现与 CC
冲突时（例如摘要请求形态），以 CC 为准、只移植思想。

## 一、为什么这样设计

本机 nyan 的 4 个真实会话里，工具结果占发给模型内容的 54%–83%（按块类型统计字符数）；nyan-legacy
在 22 个 durable 会话上测得 94.9%。两组数据的结论相同：**清掉旧工具结果是主路径，摘要是例外**。legacy
的量化还表明：阈值取单条占预算 1% 明显好于 2%/5%，"保留最近 2 轮"与"8 轮"差别很小。

CC 现状的问题：

| 现状 | 位置 | 问题 |
|---|---|---|
| time-based microcompact | `services/compact/micro_compact.rs:379-438` | 空闲 60 分钟才触发；直接改写 `messages_for_query`，不冻结、不持久化；每次清理后 `reset_microcompact_state()`，prompt cache 整段失效 |
| 工具结果替换 `ContentReplacementState` | `utils/tool_result_storage.rs:36` | 机制正确（按 id 冻结、字节一致重放、`content-replacement` 条目持久化、resume 重建），但默认关闭（`feature_flags.rs:470`），且只按单条消息 200k 字符预算触发 |
| autocompact | `services/compact/auto_compact.rs:198` | 超阈值就把全部历史交给主模型摘要；不保留任何原文尾部 |
| 摘要校验 | `services/compact/compact.rs:644-672` | 只拒绝空文本与 `API Error`；不看 `stop_reason`，截断的摘要会被当成完整摘要 |
| Read 去重 | `tools/file_read_tool/mod.rs:1038-1061` | 同文件同范围且 mtime 未变时只回"参考之前的结果"；若之前的结果已被清掉，模型拿不到内容 |

## 二、流水线

每次模型请求前（`query.rs` 中 microcompact 位置），替换 CC 的 time-based 路径：

```
boundary 之后的视图
  → measure：估算上下文 token（扣除已掩码的部分）
  → 未超阈值 ─────────────────────────────▶ continue
  → 超阈值 → mask：清可清的旧工具结果（冻结、持久化）
           → 重新 measure
           → 回到阈值内 ──────────────────▶ continue
           → 仍超 → compact：摘要切点之前的前缀，保留尾部原文
```

没有 `blocked` 状态：掩码后没降下来就走摘要；摘要失败就是失败（上下文不变、计入熔断）。

## 三、决定

### C1 掩码载体：复用 ContentReplacementState

- 不新增 entry kind、不外部化、不算哈希。原始工具结果仍在 transcript 里不动，掩码只是"不再发送"。
- 掩码决定记入 `ContentReplacementState.replacements`，以 `content-replacement` 条目持久化，resume 时由
  `reconstruct_content_replacement_state` 重建。
- 占位文本改为带元数据（legacy 格式）：

  ```
  [tool result cleared: <toolName> <bytes> bytes, id <tool_use_id>]
  ```

  CC 原有单消息预算替换的占位 `TOOL_RESULT_CLEARED_MESSAGE`（`"[Old tool result content cleared]"`，
  `utils/tool_result_storage.rs:11`，写入点 `tool_result_storage.rs:557`）也改用这个格式：同一份 state、同一条
  `content-replacement` 持久化与 resume 路径，不保留两种占位。已有 transcript 里的旧格式替换按记录原文重放
  （`reconstruct_content_replacement_state`，`tool_result_storage.rs:100`，字节一致，不改写），否则 resume 后前缀变化、cache 断。
  `tool_result_storage.rs:655`、`:821` 两处按旧常量判"已清"的比较要同时认新格式。

- 新增的掩码与 CC 原有的单消息预算替换共用同一份状态，所以开启掩码等于开启该状态（`@nyan`：
  `ToolResultContentReplacement` 在本 fork 默认开启）。
- **大 transcript 的 resume 漏洞（必须一起修）**：transcript > 5 MB 时，resume 只读最后一个**不带
  `preserved_segment`** 的 compact boundary 之后的内容（`utils/session_storage.rs:2385-2393` 遇到带 preserved_segment
  的边界不清空读取范围；`session_storage.rs:2625` 的偏移也只认无 preserved_segment 的边界），boundary 之前的条目
  只保留 `METADATA_MARKERS` 列出的类型（`session_storage.rs:5727`），其中**没有 `content-replacement`**。
  所以 C7 的 UpTo 摘要本身不丢掩码；丢失发生在**之后又发生一次无 preserved_segment 的边界**（C7 退回全量摘要、
  或 `/compact` 走全量路径）时：此前写下的 `content-replacement` 条目都在这个边界之前被丢掉，而保留尾部里仍可能
  有它们对应的工具结果（经 preserved_segment 重新接链）→ resume 后原文复活、cache 断、上下文膨胀。修法：把
  `"type":"content-replacement"` 加进 `METADATA_MARKERS`（`@nyan`）。本会话的 transcript 已 5.2 MB，这不是假设场景。
- `tool_result_storage.rs:31` 旧注释"session writes disabled"已过时（`SESSION_WRITE_ENABLED = true`，
  `session_storage.rs:86`），实现时一并更正。

### C2 触发与候选

- 触发：`measure > 阈值`，阈值沿用 autocompact 的 `有效窗口 − 13k`（`auto_compact.rs:91`）。
- 候选（全部满足）：
  1. 工具结果所属的工具在可清名单内（沿用 `is_compactable_tool_name`，`services/compact/micro_compact.rs:507`；
     检索的 `CodeSearch` 将加入该名单，由检索切片 I 负责，见 README 第四节第 5 条）；
  2. 位于本次 `query()` 调用开始时的最后一条用户消息**之前**（legacy 说的"在飞那一轮"在 CC 里就是这个边界：
     模型正在使用的本轮工具结果一律不清）；
  3. 单条估算 token ≥ 预算的 1%（预算 = 有效窗口）；
  4. 尚未被替换。
- 选取：从旧到新累加，直到预计 `measure ≤ 阈值 − 余量`（C3）；然后**把最早被清位置之后的其余候选一并清掉**。
  理由：prompt cache 是前缀匹配，本轮 cache 从最早被改动的位置起失效，这个代价已经付了；顺带清掉其后的候选不增加
  本轮的失效量，却能腾出更多空间、推迟下一次掩码（下一次掩码又会断一次 cache）。
- **谁做决定**：主线程与普通子代理（Agent 工具派生的独立上下文）照常掩码，子代理的记录带自己的 `agentId`
  （`record_content_replacement` 已支持）。**cache 共享分叉**（README 第一节第 8 条名单：`Compact`、`SideQuestion`、
  `PromptSuggestion`、`ExtractMemories`、`AutoDream`、`CacheWarm`）**只重放已有替换、不新增决定**：
  它们经 `run_forked_agent` → `spawn_query`（`utils/forked_agent.rs:557-580`）走完整 `query_loop`，并继承父线程的
  `content_replacement_state`（`forked_agent.rs:204`）；若在 `query.rs:932-1051` 的单消息预算 / microcompact 路径上
  自己做新决定，前缀就与主线程分叉、cache 失效。实现：该路径按 `is_cache_sharing_fork(query_source)`（README 第 8 条，
  `utils/forked_agent.rs`）判别，名单内只应用 state 里已有的 replacements，跳过掩码候选选择与
  `apply_tool_result_budget_with_state_and_skip_tool_names_result`（`tool_result_storage.rs:461`）的新替换。
- **决定只在一处发生**：掩码与单消息预算的新决定只在 `query_loop` 的请求前执行。`/context`
  （`commands/context/context.rs:16`）与 `/compact`（`commands/compact/compact.rs:63`）调用的
  `microcompact_messages`（`micro_compact.rs:198`）改为**纯投影**：按现有 state 应用替换，只读、不持久化、不改 state
  （`/compact` 随后的摘要走 C6 分叉，同样只重放）。

### C3 冻结与 prompt cache

prompt cache 是第一约束：**不允许任何因时间流逝而丢掉 cache 的设计**，cache 只能在"上下文内容确实变了"
时失效，并且每次失效都要换来足够的 token 节省。

- 一个 id 一旦掩码，永久掩码，后续每轮字节一致重放。cache 只在掩码发生的那一轮从最早被清的位置起失效一次，
  之后稳定。
- **CC 的 time-based microcompact 在本 fork 移除（不是关闭）**：它按"空闲 60 分钟"触发，等于把时间流逝变成
  cache 失效；且每次清理调用 `reset_microcompact_state()`。两点都与本约束冲突。
- 掩码只在**超阈值时**发生（C2），不按时间、不按轮数；一次掩码要清到 `measure ≤ 阈值 − 掩码余量`
  （余量默认为预算的 10%），避免每轮都只清一点点、每轮都打断 cache。
- 分叉与命令路径不新增决定（C2），否则同一前缀会被两处各自改写。
- cached microcompact（`NYAN_CACHED_MICROCOMPACT`，服务端 cache edit）保持原样不动，默认关闭。
- cache 保活见 C12。

### C4 估算

- `measure` = 最后一次 assistant `usage` + 其后消息的粗估（CC 现状），再减去自该 usage 之后新掩码条目的
  token 数。否则掩码后的重新 measure 仍读到掩码前的 usage，会误判"仍超"直接走摘要。
- 落盘 usage 可能缺 output 与最终 cache 拆分（停在 `message_start`，C13-1），只影响事后统计 / 命中率复盘；measure
  不受影响——`get_token_count_from_usage`（`utils/tokens.rs:28-33`）= input + cache_creation + cache_read + output，
  87 个样本中 `message_start` 与最终值的 prompt 侧完全相等，只差 output。resume 后照常用落盘 usage。保活需要 cache
  拆分，读内存里的最终 usage（C12）。

### C5 Read 去重修正

- 判断 `file_unchanged`（`tools/file_read_tool/mod.rs:1038-1061`）前，若该文件最近一次 Read 的结果已被替换
  （掩码或单消息预算），则视为未读过，正常返回内容。
- 两种做法：
  - (a) 给 `ReadFileStateEntry`（`utils/query_helpers.rs:28`）加 `tool_use_id` 字段。该结构体字面量在 `src/` 有 48 处
    构造（17 个文件，含测试），resume 时由 `extract_read_files_from_messages`（`query_helpers.rs:62`）从消息重建，
    也要补；改动面大。
  - (b) **不改 entry**：去重命中时，从 `context.messages`（`tool.rs:1261`）倒序找该路径最近一次 Read 的 tool_use，
    查它的 id 是否在 `context.content_replacement_state.replacements` 里。
- **采用 (b)**：只改 `file_read_tool/mod.rs` 一个文件；state 与 messages 本就是唯一真相，resume 后由
  `reconstruct_content_replacement_state` 重建，自然一致，不需要重建路径另行维护；倒序扫描只在去重命中时发生，
  代价可忽略。找不到对应 tool_use（例如在 boundary 之前）时按"已不在上下文"处理，同样正常返回内容。
  （注：Read 的 `call` 其实能从 `context.tool_use_id`（`tool.rs:1342`，由 `services/tools/tool_execution.rs:3117`
  写入）拿到本次 id，(a) 并非做不到，只是改动面与 resume 一致性都不如 (b)。）

### C6 摘要：legacy 的结构化格式，CC 的 cache 共享请求

只换"让模型写什么"，不换"怎么发请求"：

- **请求形态沿用 CC**：摘要是共享主会话 cache 的分叉（`compact.rs:990-1035`，`run_forked_agent`）——
  system、tools、历史消息与主会话逐字节相同，末尾追加一条摘要指令，历史部分全部命中 cache，只付输出 token。
  legacy 的"把对话序列化成 `<conversation>` 纯文本再发"会丢掉整段 cache、按全价重新输入，与 C3 的 cache 约束
  冲突，**不采用**。CC 的 fallback（`compact.rs:1087`，分叉失败时的无 cache 请求）保持原样。分叉的
  `query_source = Compact`，属 cache 共享分叉，只重放已有替换（C2）。
- **摘要指令换成 legacy 格式**（`@nyan`，替换 `base_compact_prompt.txt` 的内容）：Goal / Constraints &
  Preferences / Progress（Done / In Progress / Blocked）/ Key Decisions / Next Steps / Critical Context，
  "保留精确的文件路径、函数名、错误信息"；被摘要部分以旧摘要开头时，要求并入旧摘要。有自定义指令时追加
  `Additional focus: …`。
- CC 的 partial compact 两个模板（`partial_compact_up_to_prompt.txt` / `partial_compact_from_prompt.txt`）
  同样换成上述结构，只保留各自"只摘要切点前 / 切点后"的范围说明。
- 摘要写回格式沿用 CC（`get_compact_user_summary_message`，带 transcript 路径提示），不改。
- 回退开关：`settings.nyan.compaction.summaryStyle = "cc"` 恢复 CC 原提示词，默认 `"legacy"`。

### C7 全量摘要保留尾部：keepRecentTokens

- autocompact 不再"全部摘要"，改为 legacy 的切点选择，结果走 CC 现成的
  `partial_compact_conversation(…, UpTo)`（`compact.rs:765`），尾部原文作为 `preserved_segment`
  落盘、resume 时重新接链（`session_storage.rs:5571`）。
- 切点（legacy §8.7）：从最新往旧累加估算 token，到达 `keepRecentTokens` 的那条起，切在其后第一个候选；
  候选是以 user 或 assistant 开头的消息，不能是工具结果，也不能是"前一条 assistant 的调用结果还在其后"的
  user 消息（tool_use / tool_result 成对）。
- 默认 `keepRecentTokens = 20000`、摘要输出上限 `min(0.8 × 16384, 模型上限)`（legacy 默认值）。
- 没有可切的位置（总量不到 `keepRecentTokens` 或切点前没有内容）时退回 CC 的全量摘要。
- cache：UpTo 只发切点之前的前缀（`compact.rs:824-825`），该前缀与主会话请求的开头逐字节相同，仍走
  cache 共享分叉、按前缀命中；命中量比全量摘要少最后一个 cache 断点之后的部分，可接受。
- 手动 `/compact` 同样走这条路径；`/compact` 的消息选择器（partial compact）保持 CC 行为。

### C8 摘要校验

摘要响应必须同时满足，否则失败、上下文不变、计入熔断。

**落点**：`stream_compact_summary`（`services/compact/compact.rs:1037`，取摘要在 `:1047-1059`）现在取的是"最后一条
带文本的 assistant"——以 tool_use 结尾但前面有文字的回复会被当成摘要。校验在这里做：取分叉输出的**最后一条**
assistant，按下表判；不再向前找"有文本的那条"。fallback `stream_compact_summary_fallback`（`compact.rs:1087`）
已把 `stop_reason` 写回 assistant，同样过这张表。

| 条件 | 失败原因 |
|---|---|
| `stop_reason` 为 `EndTurn` / `StopSequence`（`types/message.rs:1183`） | `MaxTokens` → `summary_truncated`；`ToolUse` → `summary_contains_tool_call`；`Refusal` / `PauseTurn` / 缺失 → `model_error` |
| 不含 tool_use | `summary_contains_tool_call` |
| 文本非空 | `summary_empty` |
| 非 `API Error` / prompt-too-long（沿用 CC 的 PTL 截头重试） | `model_error` |

### C9 回执

写进 compact boundary 的 `CompactMetadata`（CC 已有 `trigger`、`pre_tokens`、`messages_summarized`、
`preserved_segment`），新增：

```
nyan: { maskedCount, maskedTokens, summaryModel, summaryStyle, keepRecentTokens,
        tokensAfterEstimate, usage }
```

字段落地为 `CompactMetadata`（`types/message.rs:701`）上的 `pub nyan: Option<NyanCompactReceipt>`，带
`#[serde(default, skip_serializing_if = "Option::is_none")]`（与该结构体其余可选字段一致），旧 transcript 无此字段照常解析。

掩码本身的回执即 `content-replacement` 条目（id + 占位文本），不另建文档。

### C10 摘要模型

- `settings.nyan.compaction.model`：摘要使用的模型，未设置时用当前主模型（CC 行为）。
- **代价**：prompt cache 按模型隔离，换模型就无法共享主会话 cache，摘要请求要按全价输入整段历史（走 CC 的
  无 cache fallback 路径）。主模型贵、摘要模型便宜很多时才划算；文档与 `/compact` 输出里注明本次是否命中 cache。
- 窗口检查：被摘要的输入估算超过摘要模型窗口时，先按 CC 的 PTL 截头逻辑截断，仍超则失败（不静默换模型）。

### C11 开关

`SettingsJson`（`utils/settings/types.rs:63`）是严格 schema：编辑时未知顶层键直接报错
（`utils/settings/validation.rs:454`）。所以新增一个正式字段 `nyan: Option<NyanSettings>`（`@nyan`），
压缩配置放在 `nyan.compaction` 下，后续模块（记忆、检索、外部搜索）也挂在 `nyan` 下，不散落到顶层。

| 设置 | 默认 | 作用 |
|---|---|---|
| `settings.nyan.compaction.mask` | `true` | 关闭后回到 CC：无掩码、全部摘要 |
| `settings.nyan.compaction.maskMinFraction` | `0.01` | 候选单条占预算的下限 |
| `settings.nyan.compaction.keepRecentTokens` | `20000` | 摘要保留的尾部 |
| `settings.nyan.compaction.summaryStyle` | `"legacy"` | `"cc"` 回退 CC 提示词 |
| `settings.nyan.compaction.model` | 未设置 | 摘要模型 |
| `settings.nyan.compaction.maskHeadroom` | `0.10` | 一次掩码清到阈值以下多少（占预算比例） |
| `settings.nyan.cache.warming` | `"active"` | 保活模式：`"off"` / `"active"` / `"idle"`（C12） |
| `settings.nyan.cache.minCachedTokens` | `20000` | cache 部分低于此值不保活（C12） |
| `settings.nyan.cache.maxActiveMinutes` | `60` | agent 运行中连续保活的上限（C12） |
| `settings.nyan.cache.maxIdleMinutes` | `480` | 等待用户输入后保活的上限，仅 `"idle"` 模式（C12） |

### C12 prompt cache 保活（来自 legacy cache warmer）

问题：Anthropic 的 cache TTL 默认 5 分钟（可选 1 小时）；思考一会、跑一条长命令、或离开一阵后，下一次请求
整段 cache 过期，按全价重写。

本机会话实测（按请求去重 usage 后）：

| 会话 | 模型 | 命中率 | 说明 |
|---|---|---|---|
| 66b6b0c1 | claude-opus | 98% | 10 次空闲超 5 分钟，最长 23 分钟，仍 98%：网关侧对 Claude 的 cache 寿命明显长于 5 分钟（未验证具体值） |
| 244e7657 | deepseek-flash | 98% | 无长空闲 |
| e570820f | deepseek-flash | 93% | 无长空闲 |
| 83c495ff | gpt-6.1-sol + deepseek | 71%（按最终 usage） | 原记"45%、167 次中 101 次 `cache_read=0`"是统计口径错误：208/351 个 gpt 请求的 transcript 只有 `message_start` 阶段的 usage（`cache_read=0`、`output=0`），不是真实未命中。按最终 usage：前 247 个请求约 99%；第 248 个起（约 300k token）连续只命中 12544（system + tools）直到 7.5 小时后自行恢复（见 C13） |

legacy 的思想（`packages/coding-agent/src/core/cache-warmer.ts`）：在 cache 过期前把**同一个请求原样重放、输出
上限 1 token**，续上寿命；值不值得续由经济账决定。legacy 的经济账按美元算（价格表 + $0.05 阈值 + 0.15 空闲继续概率），
在本项目不适用：CC 的价格表（`utils/model_cost.rs`）只覆盖 Claude，网关上的 GPT / DeepSeek 等查不到价格。
本项目按 **token 账**落地——对任何模型，cache 读取约为输入价的 1/10、写入约等于输入价，所以只比 token 量：

- **何时保活**：一次真实请求结束后，若它的 prompt 中 cache 部分（`cache_read + cache_creation`）≥
  `nyan.cache.minCachedTokens`（默认 20000），就排一个保活定时器；否则不值得（重写成本本来就小）。
- **TTL 从响应里读，不让用户配**：Anthropic usage 带 `cache_creation.ephemeral_5m_input_tokens` /
  `ephemeral_1h_input_tokens`（CC 已解析，`services/api/claude.rs:148`）——哪个非零就是哪个 TTL。都为零且
  `cache_read` 也为零（这个模型 / 网关没 cache）就不保活。网关若对 Claude 的实际寿命更长（本机实测空闲 23 分钟仍
  命中），按 5 分钟保活只是多花少量读取，不会出错。
- **没有 `ephemeral_*` 字段但 `cache_read > 0`**（经网关的 GPT / DeepSeek 等，由上游自动缓存）：TTL 未知，**不保活**。
  这类上游的自动缓存寿命由服务端决定、无法从响应得知，保活请求能否续命也无法验证；本机这几类会话在无长空闲时
  命中率已达 93%–99%（上表，按最终 usage）。以后若实测某个上游有固定 TTL 且保活有效，再按模型名加白名单。
- 判断用**内存里的最终 usage**（`message_delta` 合并后），不读 transcript（落盘值可能缺最终 cache 拆分，见 C13-1）。
- **时机**：TTL 的 90%（至少留 10 秒余量）发一次；定时器迟到超过余量一半就放弃（迟到的刷新等于一次全价写入）。
- **请求**：复用 CC 每轮结束时保存的主线程 cache-safe 参数（`get_last_cache_safe_params`，`utils/forked_agent.rs:404`；
  由 `query/stop_hooks.rs:91-114` 写入，CC 本来就用它让 `/btw`、prompt suggestion 等分叉共享主会话 cache）。
  保活走 `run_forked_agent`，`query_source = CacheWarm`（README 第一节第 8 条新增的枚举值，属 cache 共享分叉，适用
  那里的三条规则：不带 `X-Claude-Code-Agent-Id`（C13-3）、只重放不新增掩码（C2）、用 `is_cache_sharing_fork` 判别）：
  `max_output_tokens = Some(1)`、`max_turns = 1`、`skip_transcript = true`，追加一条极短的 user 消息。分叉请求 = 主会话前缀（system、tools、历史逐字节相同）+ 这条消息，读到整段已缓存前缀、刷新其寿命，
  只新写几个 token——与 CC 现有分叉共享 cache 的机制相同，不需要另存"最后一次请求的原始字节"。
  - **与 thinking 的兼容**：`max_output_tokens` 进入 thinking 参数（`utils/forked_agent.rs:451` 的注释已指出）。
    `local_thinking_config_to_sdk`（`services/api/claude.rs:1807`）中，`Adaptive` 只在
    `model_supports_adaptive_thinking(model, profile)`（`utils/thinking.rs:193`）为真时原样发出；否则被转成
    `Enabled { budget_tokens = min(模型上限, max_output) − 1 }`，`Enabled` 也被夹到 `max_output − 1`——1 token 时
    budget 退化为 1（低于 API 下限，非法），且与主请求的 thinking 参数不同。规则：thinking 关闭（`Disabled` 或
    `!model_supports_thinking(model, profile)`，`utils/thinking.rs:171`）或 `Adaptive` 且
    `model_supports_adaptive_thinking(model, profile)` 时才保活；`Enabled` 或会被转换的 `Adaptive` 一律不保活。
  - "thinking 参数是否进入消息级 cache key"（即 adaptive / disabled 时 `max_tokens = 1` 是否仍命中主请求的 cache）
    是切片 F 的**第一步实测**，结果决定上面规则是否需要收紧；不是待定项。
- **何时停**：上下文一变（新的真实请求、换模型、掩码、摘要）立即停；运行中最长保活 `nyan.cache.maxActiveMinutes`
  （默认 60 分钟），agent 停下（等待用户输入）后最长 `nyan.cache.maxIdleMinutes`（默认 480 分钟，即 8 小时）（legacy 的两个
  安全上限，保留——它们防的是"人走了还在一直花钱"）。空闲上限从 legacy 的 30 分钟放宽到 8 小时，依据：Opus 5.5、10 万 token
  前缀，1h cache 过期后重写约 $0.80，一次续命读取约 $0.02，8 小时内每小时一次续命仍远低于一次重写。
- **记账**：保活请求的 usage 计入 CC 现有的会话费用统计（`cost_tracker.rs`），不进模型上下文，不写成消息。
- **开关** `nyan.cache.warming`：`"off"` | `"active"`（默认，仅 agent 运行期间，覆盖长工具调用与长思考）|
  `"idle"`（等待用户输入时也保活，受 8 小时上限）。
- **1 小时 TTL**：由 profile 的 `cacheRetention` 决定，默认 `long`（Claude 每个断点显式 `ttl: "1h"`，`short` 显式
  `ttl: "5m"`）。CC 的 `promptCache1h` / 员工 / Bedrock 环境变量判断已删除。保活在 1h TTL 下自然变成每 54 分钟一次。

### C13 非 Anthropic 模型的 cache 利用（已查清，2026-10-27）

原判断"经网关调用 gpt 时 cache 基本失效"**不成立**，是统计口径错误；真实问题有两个，一个在 nyan，一个在网关。

**1. transcript 里的 usage 不是最终值（nyan 侧，统计口径）**

- 83c495ff（nyan-pc 会话）的 351 个 gpt 请求中，208 个在 transcript 里没有任何带 `stop_reason` 的条目，usage 是
  `message_start` 阶段的值（`cache_read=0`、`output=0`）；这 208 个全部以 `tool_use` 结尾、紧跟 `tool_result`。
- 机制：主会话的 assistant 记录入写队列后，由 `message_delta` 的 `AssistantDelta` 回写修补
  （`query.rs:2132-2172` → `session_storage.rs:3870` `apply_assistant_delta_to_queued_record`）。CLIProxyAPI 会提前推出
  tool_use 块，流式执行器立即开跑工具，`tool_result` 记录先入队，delta 晚到时修补不到——CC 自己也接受这个竞态
  （100 ms 刷新，`session_storage.rs:3936-3938` 注释）。本机直连 Claude / DeepSeek 的会话里每会话只有 0–4 个这样的请求。
- **影响范围只在落盘的数字**：落盘 usage 可能缺 output 与最终 cache 拆分，只影响事后统计 / 命中率复盘。measure 不受
  影响（含 resume 后）：`get_token_count_from_usage`（`utils/tokens.rs:28-33`）是 input + cache_creation + cache_read
  + output 之和，87 个样本中 `message_start` 与最终值的 prompt 侧完全相等，只差 output。保活需要 cache 拆分，读内存里
  的最终 usage。
- **决定**：
  - 不改写队列的竞态（与 CC 行为一致，改动面大、收益只在统计）；本文与后续统计脚本**只用带 `stop_reason` 的条目**
    计算命中率。

**2. 第 248 个请求起的命中断崖（网关侧，不在 nyan 修）**

- 第 248 个 gpt 请求（约 300k token）起，`cache_read` 连续固定为 12544（只命中 system + tools），未缓存 290k–330k，
  请求间隔 10–60 s、未换模型、未压缩；约 7.5 小时后自行恢复到 308k 命中。
- 已排除：`prompt_cache_key` 不稳定。CLIProxyAPI（v8.0.44 源码）的 Codex executor 取 `X-Claude-Code-Session-Id` +
  `X-Claude-Code-Agent-Id` 生成 `prompt_cache_key`（`helps/claude_code_session.go:120-127`、
  `codex_executor_request.go:117-153`），会话内稳定。
- 未验证的假设：(a) 上游把请求路由到另一账号 / 机器，或 cache 被淘汰；(b) CPA 的 reasoning replay 每 agent 上限
  256 turns / 16 MB（`internal/cache/codex_reasoning_replay_cache.go:31-34`），超限从头部裁剪旧 turn，改变了请求前缀。
  需要 CPA 日志（trace-id、auth 切换、replay 裁剪）核实，属于网关运维，不进入任何 nyan 切片。

**3. 子代理与主线程共用网关 cache 身份（nyan 侧，需修）**

- CPA 的 `prompt_cache_key` = `sha1(model, "claude:<sessionId>:agent:<agentId>")`，`agentId` 只取 `X-Claude-Code-Agent-Id`
  头、缺失即 `main`（`helps/claude_code_session.go:31-45,120-127`）。nyan 不发这个头（全仓无此字符串；CC 原版是否发未核实），
  所以所有子代理与主线程共用同一个 `prompt_cache_key` 与 reasoning replay 状态。前缀不同的请求挤同一个 key，会互相顶掉 cache，也会让 replay 把子代理
  的 reasoning 混进主线程。
- **决定**：子代理请求（`run_agent` / `run_forked_agent`）带 `X-Claude-Code-Agent-Id: <agentId>`，主线程不带（CPA 缺省为
  `main`）。**例外**：cache 共享分叉（README 第一节第 8 条名单，`is_cache_sharing_fork` 判别）**不带**——它们的
  前缀与主线程逐字节相同，目的就是命中主线程的 cache。
- 对 Anthropic 直连无影响（该头不进 cache key）。这一项是独立的小切片，见第五节切片 G。

### C14 决策点（Decision Provider）

| 决策点 | 确定性规则 | Decision Provider（decision-provider.md 4.4） |
|---|---|---|
| 掩码候选的**清理顺序** | C2：从旧到新累加；最早被清位置之后的候选一并清 | `compaction.keep`（shadow）：见下 |
| 清多少 / 何时掩码 | C2 / C3：清到 `measure ≤ 阈值 − maskHeadroom`，只在超阈值时 | 不接入（token 账精确，是 cache 约束的核心） |
| 是否直接摘要 | 先掩码再 measure，仍超才摘要 | 不接入 |
| 摘要切点 | C7 `keepRecentTokens`，tool_use / tool_result 成对 | 不接入（成对必须可证明） |
| 保活是否值得 | C12 token 账 | 不接入 |

**`compaction.keep` 的语义**：在一次掩码发生的那一轮，对 C2 的候选批量问"这条旧工具结果对当前任务是否仍然需要"（输入：
当前任务摘要 + 每个候选前 1500 字符）。判"仍需要"的候选从本轮的清理序列中**移到末尾**：

- 先按 C2 从旧到新只清"不再需要"的候选，累加到 `阈值 − maskHeadroom`；不够再按从旧到新清"仍需要"的。**清理总量不变**。
- C2 的"最早被清位置之后的候选一并清"改为：只对"不再需要"的候选适用；"仍需要"的不因此被顺带清掉。
- cache：前缀失效从"最早被清的位置"开始；把仍需要的旧结果留下，最早被清的位置不会更早，失效量相同或更小。
- 决定随 `content-replacement` 条目持久化，之后字节稳定、不重复询问（C3）。cache 共享分叉不发起这个决策（README 第一节第 8 条）。
- Decision Provider 不可用或超时（默认 800 ms，配置位于 `settings.nyan.decision.deadlineMs.critical`；因 `compaction.keep` 属于关键路径决策点，由 critical deadline 控制）：按 C2 原规则。默认 shadow：后台 fire-and-forget 异步评估（不阻塞主 query 循环），先观察"被留下的结果之后是否被模型引用"，再决定是否 active。

## 四、明确不做

| 项 | 理由 |
|---|---|
| `basis` / `stale_basis` | 为 durable 的异步压缩任务设计；CC 的 autocompact 在请求前同步执行，没有并发写入 |
| `blocked` 准入状态 | legacy 已砍：没有真正的产生者 |
| artifact store、哈希绑定掩码 | 条目不可变且永存，恢复路径就是 transcript 本身 |
| ContextUnitManifest、22 种 AnchorKind、8 种压缩动作、CDR/CAR、审批门 | legacy 已明确后置；没有 Kernel / RunLedger / 审批载体 |
| context collapse、snip、reactive compact | CC 上游也是空 stub，保持 no-op |

## 五、实现切片

| 切片 | 内容 | 主要写集 | 依赖 |
|---|---|---|---|
| A | 掩码：候选筛选、冻结、新占位格式（含单消息预算占位，旧格式按原文重放）、持久化；cache 共享分叉只重放（`is_cache_sharing_fork`）；`/context`、`/compact` 的 `microcompact_messages` 改为只读投影；移除 time-based 路径；开启 ContentReplacementState；`METADATA_MARKERS` 加 `content-replacement` | `services/compact/micro_compact.rs`、删除 `services/compact/time_based_mc_config.rs` 与 `services/compact/mod.rs:14` 的声明、`utils/tool_result_storage.rs`、`utils/feature_flags.rs`、`utils/session_storage.rs`、`query.rs`（`:932-1051` 的分叉判别）、`commands/context/context.rs`、`commands/compact/compact.rs` | 批次 0（`is_cache_sharing_fork`） |
| B | measure 扣除掩码 token；准入 continue/mask/compact 接线 | `utils/tokens.rs`、`services/compact/auto_compact.rs`、`query.rs` | A |
| C | Read 去重修正：去重命中时从 `context.messages` 反查最近一次 Read 的 id 是否已被替换（方案 b，不改 `ReadFileStateEntry`） | `tools/file_read_tool/mod.rs` | A |
| D | legacy 摘要格式；校验落在 `stream_compact_summary` 与 fallback；回退开关、摘要模型配置 | `services/compact/compact.rs`、`services/compact/prompt.rs` | — |
| E | keepRecentTokens 切点 + 走 partial compact UpTo；回执（`CompactMetadata.nyan: Option<_>`） | `services/compact/auto_compact.rs`、`services/compact/compact.rs`、`types/message.rs` | B、D |
| F | cache 保活：第一步实测 thinking 参数与消息级 cache key；从 usage 读 TTL、token 账、thinking 兼容判定（`model_supports_adaptive_thinking`）、定时器、`QuerySource::CacheWarm` 分叉重放、费用记账、四项 `nyan.cache.*` 配置 | 新 `services/cache_warmer.rs`、`constants/query_source.rs`（`CacheWarm`）、`query/stop_hooks.rs`（启动）、`query.rs`（新请求时停止）、`cost_tracker.rs` | 批次 0 |
| G | 网关 cache 身份（C13-3）：普通子代理请求带 `X-Claude-Code-Agent-Id`；cache 共享分叉不带 | `services/api/claude.rs`（按请求加头，`agent_id` 已在 options 里，`claude.rs:411`）、`utils/forked_agent.rs` | — |

A、C、D、F、G 可并行（写集不重叠；A 与 F 都依赖批次 0 的 `is_cache_sharing_fork`；`query.rs` 中 A 改
`:932-1051`、F 改新请求入口，hunk 不重叠）；B 在 A 后；E 最后。

切片 A 移除 time-based 的连带面（`rg -n 'time_based|TimeBased' src` 核实；`components/spinner/mod.rs` 的
`time_based_tips_enabled` 是无关的提示开关，不动）：
- `services/compact/micro_compact.rs`：`TIME_BASED_MC_CLEARED_MESSAGE`（`:22`）、`microcompact_messages_with_time_config`
  （`:217`）、`microcompact_messages_with_configs` 的 `time_config` 参数与 `maybe_time_based_microcompact` 调用（`:231-239`）、
  `maybe_time_based_microcompact`（`:379`，含 `:435` 的 `reset_microcompact_state()`）、`evaluate_time_based_trigger`（`:467`），
  以及 `:616`、`:692`、`:787`、`:898`、`:1093` 五个 time-based 测试（删除，改为断言"空闲后不清理"）；
- `services/compact/time_based_mc_config.rs` 整个模块（`TimeBasedMicrocompactConfig`、`time_based_microcompact_config_from_env`
  及其测试；env 覆盖 `NYAN_TIME_BASED_MICROCOMPACT`、`CLAUDE_CODE_TIME_BASED_MICROCOMPACT`、`..._GAP_MINUTES`、
  `..._KEEP_RECENT` 随之失效）与 `services/compact/mod.rs:14` 的 `pub mod`；
- `utils/feature_flags.rs`：`FeatureFlag::TimeBasedMicrocompact`（`:31`）及其 `FeatureSwitch`（`:223`）。

切片 G 的判别：`run_forked_agent` 给每个分叉也分配 `agent_id`（`forked_agent.rs:199`），所以不能按"有无 `agent_id`"
决定是否带头；按 `is_cache_sharing_fork(query_source)`（README 第一节第 8 条名单）判别，名单内不带，其余子代理
（Agent 工具派生的独立上下文）带。每个切片：受影响测试改为断言新行为，新增行为各有测试；
`just test` 的隔离方式跑受影响模块；用真实会话回放验证掩码后的 token 曲线。

## 六、验收

1. 超阈值时先掩码；掩码后回到阈值内则不调用摘要模型。
2. 同一 id 掩码后，后续每轮发出的字节一致（cache 不再失效）。
3. resume 后掩码状态一致；transcript 里原始工具结果仍在；transcript > 5 MB、先 UpTo 摘要（带 preserved_segment）
   再发生一次全量摘要（无 preserved_segment）后，保留尾部里已掩码的结果仍是占位。
4. 在飞那一轮的工具结果永不被掩码。
5. 掩码后 measure 下降（不读到旧 usage）。
6. 被掩码（或被单消息预算替换）的文件再 Read 能拿到内容；resume 后同样成立。
7. 截断 / 含 tool call（包括"有前文、以 tool_use 结尾"）/ 空摘要都失败，上下文不变；分叉与 fallback 两条路径都校验。
8. 摘要后保留 `keepRecentTokens` 尾部原文，tool_use/tool_result 不被拆开；resume 后链完整。
9. 关闭 `mask` 与 `summaryStyle="cc"` 时行为回到 CC。
10. 移除 time-based microcompact 后，空闲任意时长再继续，不会因 nyan 自身行为清理任何内容。
11. 保活刷新的请求与上一次真实请求除 `max_tokens` 外逐字节相同；刷新后下一次真实请求 `cache_read` > 0。
12. 上下文变化（新消息、换模型、掩码、摘要）后保活立即停止；`warming: "off"` 时从不发刷新；无 cache 的模型从不保活；
    超过 `maxActiveMinutes` / `maxIdleMinutes` 后停止。
13. cache 共享分叉（名单内任一 `QuerySource`）运行后，`ContentReplacementState` 不变、不写 `content-replacement` 条目，
    发出的前缀与主线程逐字节相同；普通子代理照常掩码，记录带自己的 `agentId`。
14. `/context`、`/compact` 前后 `ContentReplacementState` 与 transcript 的 `content-replacement` 条目不变。
15. 保活只在 thinking 关闭或原生 adaptive 时发出；`Enabled` 或会被转换的 `Adaptive` 从不保活。
16. 旧 transcript（旧占位格式、`CompactMetadata` 无 `nyan` 字段）resume 后逐字节重放、正常解析。
