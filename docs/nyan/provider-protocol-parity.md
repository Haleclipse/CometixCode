# nyan Provider 协议对齐审计报告 (对标 Pi Agent)

> **审计状态**：只读审计产出（2026-11-04）  
> **nyan 基准**：`docs/provider-protocol-parity` 分支基于 `origin/master 102e4644f`（PR 1-8、#47、#48 已合并）  
> **Pi Agent 参考源码**：`earendil-works/pi`（commit `676af1d777ef6f04b6bc8daf4cf3b9b1799cff4d`，浅克隆于 `/home/yuki/Projects/ref/pi`）  
> **审计范围**：Anthropic Messages、OpenAI Chat Completions、OpenAI Responses（含 Codex/ChatGPT）、Google Gemini、本地通用模型（openaiChat）

---

## 0. 执行摘要与核心发现

本次审计对照 Pi Agent 的成熟实现，全面审视了 nyan 当前各 provider adapter 在**缓存机制**、**前缀稳定性**、**thinking/reasoning**、**工具调用与 Schema**、**图片输入**、**Usage 与缓存命中统计**、**Stop Reason** 以及**错误映射** 8 个维度的协议适配差距。

### 最关键发现：
1. **Anthropic 工具断点缺失与工具结果错误图片崩溃**（P0）：
   - nyan 在 `src/utils/api.rs` 中将工具的 `cache_control` 硬编码为 `None`，导致工具列表无法形成独立缓存断点；
   - 当工具调用失败（`is_error: true`）且包含图片时，nyan 直接在 `tool_result` 块中发送 `image` 块，触发 Anthropic API 的 400 校验错误（Anthropic 显式禁止在 error tool result 中携带 image）；而 Pi 通过 `splitErrorToolResultImages` 将图片优雅拆分至随后的 user 消息。
2. **OpenAI Chat Completions 缓存字段与推理流缺失**（P0）：
   - nyan 在 `src/services/provider/openai/chat.rs` 中完全未发送 `prompt_cache_key` 与 `prompt_cache_retention`；
   - nyan 仅解析 `/prompt_tokens_details/cached_tokens`，遇到 DeepSeek（`prompt_cache_hit_tokens`）、Kimi（顶层 `cached_tokens`）或 OpenRouter（`cache_write_tokens`）时，缓存命中统计永远为 0；
   - 流式解析中未捕获 `delta.reasoning_content`，且在 assistant 历史回传中直接丢弃了 reasoning 内容，导致 DeepSeek-R1 等本地/兼容推理模型在多轮交互中上下文断裂。
3. **OpenAI Responses 缓存键安全与 Codex 增量缺失**（P1）：
   - `prompt_cache_key` 仅在 Codex 后端发送，且未按 OpenAI 规范截断至 64 字符（超长会触发 400）；标准 API 后端未发送该键；
   - Codex 后端未实现 Pi 的 `previous_response_id` 机制，每轮全量回传 messages，消耗大量带宽与首字延迟。
4. **Google Gemini 3 思维参数不匹配与多模态工具结果丢失**（P0 / P1）：
   - nyan 仅支持数字/自适应 `thinkingBudget`，未适配 Gemini 3 系列（如 `gemini-3-pro`、`gemini-3.8-flash`）的离散 `thinkingLevel`（`MINIMAL` / `LOW` / `MEDIUM` / `HIGH`）；
   - 在 `src/services/provider/gemini/wire.rs` 中，工具执行结果（`ToolResult`）中的图片被直接丢弃，仅提取纯文本。
5. **系统提示词动态信息破坏通用前缀缓存**（P0）：
   - nyan 的 `compute_simple_env_info`（包含当前 `cwd`、`is_git` 等）在非 FirstParty Anthropic 模式下未被边界有效切分，直接拼入 system prompt，导致切换目录或子 worktree 时跨会话缓存击穿。

---

## 1. 全局架构对比：请求前缀稳定性与缓存模型

Prompt 缓存命中率的核心在于**请求前缀（System Prompt + Tools + 早期对话历史）的绝对字节稳定性**。

### 1.1 系统提示词分层与动态内容位置
- **nyan 现状** (`src/constants/prompts.rs:107-164`, `src/utils/api.rs:277-350`):
  - 系统提示词定义了 `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 分隔符。
  - 但 `should_use_global_cache_scope()` 仅在 `get_api_provider() == ApiProvider::FirstParty` 且未禁用实验性 beta 时返回 true。
  - 对于所有 3P Anthropic（Bedrock/Vertex/Foundry）、OpenAI、Gemini 和本地模型，该边界不生效；动态 sections（`env_info_simple` 中的 `cwd`、`is_git`，`mcp_instructions` 等）直接通过 `\n\n` 与前置静态指令合并为单一字符串。
  - 一旦用户切换工作区目录或启动子 agent，静态前缀字符串被彻底修改，引发全量缓存失效。
- **Pi 的做法** (`packages/coding-agent/src/core/system-prompt.ts:128-193`):
  - 系统提示词结构化拆解为有序 section 字典（`SystemPromptSections`）：
    - `preamble`（助手核心身份，纯静态）
    - `tools`（静态工具简述）
    - `rules`（通用行为准则）
    - `docs`（Pi 规范文档说明）
    - `addendum`（用户静态自定义提示词）
    - `project_context`（`AGENTS.md` / 项目规则）
    - `skills`（用户技能声明）
    - `cwd`（当前工作目录：单独包裹在 `<cwd>` 标签中放置在提示词末尾）
  - 动态时间戳（如当前日期/时间）**坚决不放入**系统提示词，保持前缀稳定。
- **改进建议**：
  - 将所有 provider 统一解耦为“静态不可变前缀”和“会话可变后缀”；
  - 将 `cwd`、环境信息收尾，确保前缀前 N 个 Token 在同项目下永远保持 100% 相同。

### 1.2 工具定义确定性与动态工具占位符
- **nyan 现状** (`src/services/provider/openai/mod.rs:179-184`, `src/services/provider/gemini/wire.rs:203-221`):
  - 过滤掉 `ToolSearch` 后直接序列化 tools 列表。顺序取决于内部注册表顺序，未显式做确定性排序。
  - 在动态加载工具（MCP 动态发现、ToolSearch 延迟加载）时，工具列表长度和内容直接发生改变，导致紧随其后的缓存失效。
- **Pi 的做法** (`packages/ai/src/api/anthropic-messages.ts:217-222, 1522-1526`):
  - 在支持动态工具变更的场景下，首轮请求即在 tools 尾部注册占位符工具 `__pi_deferred_placeholder__` (`defer_loading: true`)。
  - Anthropic 服务端对含有 deferred tool 的提示词会生成固定的 internal scaffolding。后续动态通过 `tool_addition` / `tool_removal` 变更工具时，底层 prompt scaffolding 不变，测得缓存命中率从 0% 恢复为接近 100%。

### 1.3 缓存浪费指标监测与统计
- **Pi 的做法** (`packages/coding-agent/src/core/cache-stats.ts:14-90`):
  - 维护 `detectMiss` 机制：
    `missedTokens = Math.min(prev.promptTokens, promptTokens) - usage.cacheRead;`
  - 过滤掉低于 1024 tokens 的断点对齐噪声（`NOISE_FLOOR_TOKENS`）；
  - 统计单会话内浪费金额（`missedCost`）与未命中轮次（`missCount`），在 CLI/UI 明确提示因 TTL 超时（> 5min）或模型切换导致的缓存击穿。

---

## 2. Anthropic Messages 协议对齐审计

### 2.1 缓存控制断点 (Breakpoints) 位置与数量
- **nyan 现状** (`src/services/api/claude.rs:1285-1376`, `src/utils/api.rs:245,262`):
  - 系统提示词：1P 模式下拆分为 1 个全局断点；3P 模式下为 1 个默认断点。
  - 工具列表：`api.rs` 中 `BetaTool.cache_control` 与 `Tool.cache_control` **均硬编码为 `None`**。整个工具集没有缓存断点。
  - 消息列表：`add_cache_breakpoints` 仅在最后一条消息（或 `skip_cache_write` 时的倒数第二条消息）放置 1 个断点。
  - **总断点数**：仅 2 个（System 1 个 + 消息尾部 1 个）。
- **Pi 的做法** (`packages/ai/src/api/anthropic-messages.ts:1485-1509, 1791-1816, 1894-1932`):
  - 系统提示词：OAuth 模式在 Claude Code identity 与 system prompt 各打 1 个断点，共 2 个；非 OAuth 模式在 system prompt 打 1 个断点。
  - 工具列表：在工具列表的**最后一个工具**（`index === tools.length - 1`）打 1 个断点（`cache_control: { type: "ephemeral", ...(ttl && { ttl }) }`）。
  - 消息列表：在最后一条 user 或 system 消息末尾打 1 个滚动断点。
  - **总断点数**：合理分配 3~4 个断点（System + Tools + Conversation Tail）。
- **建议改法**：
  - 在 `src/utils/api.rs` 或 Anthropic 序列化流程中，为 `tools` 列表的最后一个元素赋予 `cache_control`；
  - 形成标准的三段式断点：`[System Prompt 末尾] -> [Tools 列表末尾] -> [上一轮/当前轮 User 消息末尾]`。
- **优先级**：`P0`（直接影响首轮之后工具与系统的独立复用）
- **预期影响与验证**：
  - 保证多轮对话中，即使系统提示词与工具集超过几千 tokens，也能 100% 命中缓存；
  - 验证方式：检查返回 `usage.cache_read_input_tokens`，首轮后应立即接近 `system + tools` 的 token 总量。

### 2.2 缓存 TTL 控制 (5m vs 1h)
- **nyan 现状**（已由 cache-long-retention PR 取代：TTL 只看 profile `cacheRetention`，默认 long）（`src/services/provider/anthropic/cache.rs:119-220`）:
  - 1h 缓存受限严重：仅在 Bedrock 显式开启环境变量 `ENABLE_PROMPT_CACHING_1H_BEDROCK`，或 1P 且 `PromptCache1hConfig` 开启且在 `promptCache1h.allowlist` 匹配时才设置 `ttl: "1h"`。
  - 普通 API key 用户无法配置 long retention。
- **Pi 的做法** (`packages/ai/src/api/anthropic-messages.ts:77-101`):
  - 支持通过选项 `cacheRetention` 或环境变量 `PI_CACHE_RETENTION=long` 统一声明；
  - 只要模型元数据 `supportsLongCacheRetention` 为 true，自动输出 `cache_control: { type: "ephemeral", ttl: "1h" }`。
- **建议改法**：
  - 在 profile 配置中允许声明 `cache_retention: "long"` 或读取通用环境变量，并在支持 1h TTL 的模型上输出。
- **优先级**：`P1`
- **预期影响与验证**：长达 1 小时的会话间隔无需重新预热；验证 `cache_read_input_tokens` 在 5 分钟闲置后依然生效。

### 2.3 动态工具增删与占位符前缀保护
- **nyan 现状** (`src/services/api/claude.rs:2000-2014`):
  - 当通过 `tool_search` 动态发现新工具并追加至 `sdk_tools` 时，工具列表产生物理插入，导致工具列表尾部的哈希全量变更。
- **Pi 的做法** (`packages/ai/src/api/anthropic-messages.ts:217-222, 1522-1526`):
  - 初始请求即注入 `__pi_deferred_placeholder__` (`defer_loading: true`)，使服务端内部提示词预留结构，后续动态 `tool_addition` / `tool_removal` 不破坏缓存前缀。
- **建议改法**：
  - 在支持动态工具加载且启用 tool search 时，在 tools 数组末尾保留固定 deferred tool 占位符。
- **优先级**：`P1`
- **预期影响与验证**：首次执行 `ToolSearch` 展开新工具后，下一次调用不再引发整个系统提示词+工具前缀的全量 cache miss。

### 2.4 Thinking / Reasoning 机制与多轮回传
- **nyan 现状** (`src/services/api/claude.rs:2115-2135`, `src/services/provider/anthropic/wire.rs:44-55`):
  - 支持 `adaptive` 与 `budget` 模式；
  - 支持 `thinking.signature` 抓取并随 `AssistantPart::Thinking` 回传；
  - 支持 `interleaved-thinking-2025-05-14` 与 `redact-thinking-2026-03-12`。
- **Pi 的做法** (`packages/ai/src/api/anthropic-messages.ts:470-520`):
  - 支持 `thinkingDisplay`: "summarized" | "omitted"（对 Opus 4.7+ 显式默认 summarized 保持跨模型体验统一，或在极速模式下 omitted 降低 TTFT）；
  - 支持 `allowEmptySignature`：针对非官方兼容网关返回空签名的容错，避免本地验证崩溃。
- **建议改法**：
  - 完善 `thinking.signature` 为空字符串时的宽容回传逻辑。
- **优先级**：`P2`

### 2.5 图片输入：错误工具结果中的图片严重 Bug
- **nyan 现状** (`src/services/api/claude.rs:645-725`):
  - 在 `tool_result_api_content` 中，当 `is_error: true` 时，如果 `result.content_blocks` 包含 image，nyan 仍然直接将其序列化到 `tool_result` 块的 content 中。
  - **严重 Bug**：Anthropic 官方 API 明确禁止在 `tool_result` 的 `is_error: true` 块中附带图片，服务端会直接拒绝请求报 HTTP 400（`Invalid request: images are not supported in error tool results`）。
- **Pi 的做法** (`packages/ai/src/api/transform-messages.ts:31-40`):
  - `splitErrorToolResultImages` 函数：在组装请求前扫描所有 toolResult，如果 `msg.isError` 且含有 image，将该 toolResult 中的图片替换为占位提示文本 `(image attached after the tool results)`，并将原始图片以独立块的形式附加到紧随其后的同一条 user 消息末尾。
- **建议改法**：
  - 在 `src/services/api/claude.rs` 或 IR 消息投影层引入类似 `split_error_tool_result_images` 的预处理，剥离错误工具结果中的图片并移至随后的 user message。
- **优先级**：`P0`（导致命令或工具执行失败且截获图片时整个会话直接崩溃，无法继续对话）
- **预期影响与验证**：修复工具报错带图场景下的 400 崩溃；单元测试断言 `is_error: true` 的 tool_result 块绝不含 image 子块。

### 2.6 Message Threads Beta (`message-threads-2026-08-12`)
- **nyan 现状**：全量回传 messages，依赖 prompt caching 降低服务端计算成本。
- **Pi 的做法** (`packages/ai/src/api/anthropic-messages.ts:250-440`):
  - 实现了 `prepareThreadRequest` 与 `sendThreadRequest`：在 Anthropic 1P 端点上，请求带 `thread: { type: "continue", previous_message_id }`，仅发送增量新消息。遇到 `thread_not_found` 或不支持错误时自动降级回全量发送。
- **建议改法**：列入“待 Yuki 决定”事项，评估是否引入此增量协议。
- **优先级**：`P2`

---

## 3. OpenAI Chat Completions 协议对齐审计 (含通用本地模型)

### 3.1 显式缓存标记：`prompt_cache_key` 与 `prompt_cache_retention`
- **nyan 现状** (`src/services/provider/openai/chat.rs:178-247`):
  - `build_body` 仅包含 `model`、`messages`、`stream`、`max_tokens/max_completion_tokens`、`stream_options`、`tools`、`reasoning_effort`、`temperature`、`response_format`。
  - **完全没有生成** `prompt_cache_key` 字段！
  - **完全没有生成** `prompt_cache_retention` 字段！
- **Pi 的做法** (`packages/ai/src/api/openai-completions.ts:806-817`):
  ```typescript
  prompt_cache_key:
      (model.baseUrl.includes("api.openai.com") && cacheRetention !== "none") ||
      (cacheRetention === "long" && compat.supportsLongCacheRetention)
          ? clampOpenAIPromptCacheKey(options?.sessionId)
          : undefined,
  prompt_cache_retention: cacheRetention === "long" && compat.supportsLongCacheRetention ? "24h" : undefined,
  ```
  - Pi 针对官方 `api.openai.com` 且开启缓存时，自动传递会话级别的 `prompt_cache_key`（截断至 64 字符）；
  - 支持在具备 long retention 权限的端点上传递 `prompt_cache_retention: "24h"`，将缓存持久保留 24 小时。
- **建议改法**：
  - 在 `src/services/provider/openai/chat.rs` 的 `build_body` 中，当目标为 OpenAI 官方或支持 key 的端点时，附加截断至 64 字节的 `prompt_cache_key`；
  - 允许配置 `prompt_cache_retention: "24h"`。
- **优先级**：`P0`（OpenAI 官方端点前缀缓存路由与隔离的关键）
- **预期影响与验证**：OpenAI 官方端点命中率稳定提升；通过 `prompt_tokens_details.cached_tokens` 检查命中情况。

### 3.2 兼容提供商 (OpenRouter/Fireworks) 的 Anthropic 风格 `cache_control`
- **nyan 现状** (`src/services/provider/openai/chat.rs`):
  - 仅支持标准的 OpenAI JSON 字段，未对 OpenRouter、Fireworks 等网关支持的 Anthropic 风格 `cache_control` 块做适配。
- **Pi 的做法** (`packages/ai/src/api/openai-completions.ts:1062-1120`):
  - 当 `compat.cacheControlFormat === "anthropic"` 时，调用 `applyAnthropicCacheControl`：
    - 在 system/developer 消息添加 `cache_control: { type: "ephemeral" }`；
    - 在 tools 数组的最后一个工具添加 `cache_control`；
    - 在对话历史的最后一条 user/assistant 消息添加 `cache_control`。
- **建议改法**：
  - 在 `Target` 中增加 `compat.cache_control_format` 选项，若为 `"anthropic"` 则在 Chat Completions 的消息与工具中注入 `cache_control`。
- **优先级**：`P1`
- **预期影响与验证**：OpenRouter 上运行 Claude 或支持显式标记的模型时，缓存命中率从 0% 提升至 80%+。

### 3.3 Reasoning / Thinking 流式解析与跨轮保留
- **nyan 现状** (`src/services/provider/openai/chat.rs:135-154, 384-415`):
  - **流接收**：`ChatStream` 仅监听 `choice.delta.content`，对 `reasoning_content`、`reasoning`、`reasoning_text` 等字段完全忽略。
  - **多轮发送**：`push_assistant` 中显式忽略了所有 `AssistantPart::Thinking` 与 `RedactedThinking`。
- **Pi 的做法** (`packages/ai/src/api/openai-completions.ts:575-608, 1328-1381`):
  - **流接收**：定义 `const reasoningFields = ["reasoning_content", "reasoning", "reasoning_text"]`，优先抓取非空的推理字段推入 `thinking_delta`；
  - **多轮发送**：当模型声明 `requiresReasoningContentOnAssistantMessages` 时（如 DeepSeek-R1、Qwen 满血版），Pi 将之前的 thinking 文本作为 `assistantMsg.reasoning_content` 重新塞回历史消息中。
- **建议改法**：
  - 在 `ChatStream` 中增加对 `choice.delta` 中 `reasoning_content` / `reasoning` 的解析；
  - 在 `push_assistant` 中支持将 Thinking 作为 `reasoning_content` 字段回传给需要的模型。
- **优先级**：`P0`（影响 DeepSeek-R1 和本地 QwQ/DeepSeek 推理模型的思考输出与多轮正确性）
- **预期影响与验证**：终端可实时看到 OpenAI 兼容推理模型的思考过程；多轮对话不会出现推理链缺失报错。

### 3.4 Usage 与各厂商缓存命中字段解析
- **nyan 现状** (`src/services/provider/openai/chat.rs:348-360`):
  ```rust
  fn parse_usage(usage: &Value) -> Usage {
      let prompt = super::u64_at(usage, "/prompt_tokens");
      let cached = super::u64_at(usage, "/prompt_tokens_details/cached_tokens");
      Usage {
          input_tokens: prompt.saturating_sub(cached),
          output_tokens: super::u64_at(usage, "/completion_tokens"),
          cache_read_tokens: cached,
          reasoning_tokens: super::u64_at(usage, "/completion_tokens_details/reasoning_tokens"),
          ..Usage::default()
      }
  }
  ```
  - **严重缺陷**：仅匹配了 `/prompt_tokens_details/cached_tokens`（OpenAI 标准格式）。
  - DeepSeek 返回的是 `/prompt_cache_hit_tokens`；
  - Kimi / Moonshot 返回的是顶层 `/cached_tokens`；
  - OpenRouter 返回 `/prompt_tokens_details/cache_write_tokens`。
  - nyan 对以上三个高频端点返回的缓存命中均解析为 0！
- **Pi 的做法** (`packages/ai/src/api/openai-completions.ts:1521-1533`):
  ```typescript
  const cacheReadTokens =
      rawUsage.prompt_tokens_details?.cached_tokens ??
      rawUsage.prompt_cache_hit_tokens ??
      rawUsage.cached_tokens ?? 0;
  const cacheWriteTokens = rawUsage.prompt_tokens_details?.cache_write_tokens || 0;
  ```
- **建议改法**：
  - 在 `chat.rs` 的 `parse_usage` 中按顺序尝试读取：
    1. `/prompt_tokens_details/cached_tokens`
    2. `/prompt_cache_hit_tokens`
    3. `/cached_tokens`
    作为 `cache_read_tokens`，并支持读取 `/prompt_tokens_details/cache_write_tokens`。
- **优先级**：`P0`（缓存审计与统计正确性的基础；否则在 DeepSeek/Kimi 等模型上永远看不到缓存命中）
- **预期影响与验证**：运行 DeepSeek/Kimi 时，CLI 与日志能够真实显示命中上万 tokens 缓存。

---

## 4. OpenAI Responses 协议对齐审计 (含 Codex/ChatGPT 后端)

### 4.1 缓存控制：`prompt_cache_key` 与 64 字符安全截断
- **nyan 现状** (`src/services/provider/openai/responses.rs:264-266`):
  ```rust
  if backend == Backend::Codex {
      body["prompt_cache_key"] = json!(crate::bootstrap::state::get_session_id());
  }
  ```
  - 仅在 `Backend::Codex` 时设置了 `prompt_cache_key`，`Backend::Api`（使用 API Key 调用官方 Responses）完全未设置！
  - Codex 后端发送时，直接透传 `get_session_id()`，**未做 64 字符截断**。如果 session_id 超过 64 字符（例如带自定义后缀或复合路径），OpenAI Responses 服务端会直接报错 400（`prompt_cache_key exceeds maximum length of 64 characters`）。
- **Pi 的做法** (`packages/ai/src/api/openai-responses.ts:367`, `openai-prompt-cache.ts:1-8`):
  - 定义 `clampOpenAIPromptCacheKey` 严格截断字符至 64 字符；
  - 无论是 API 还是 Codex，只要缓存未禁用，均发送截断后的 `prompt_cache_key`；
  - 支持 `prompt_cache_retention: "24h"`。
- **建议改法**：
  - 统一为 `Backend::Api` 和 `Backend::Codex` 提供截断后的 `prompt_cache_key`，确保绝不超长。
- **优先级**：`P0`（既解决长 session 偶发 400 崩溃，又开启 API 端的 prompt caching）
- **预期影响与验证**：消除潜在 400 崩溃，开启 Responses API 官方路由缓存。

### 4.2 Codex 增量对话续发 (`previous_response_id` 与 WebSocket continuation)
- **nyan 现状** (`src/services/provider/openai/responses.rs:174-209`):
  - 每次请求都全量遍历 `request.messages` 重新构建 `input` 数组（包含所有先前的 `function_call`、`function_call_output` 和 `message`）。
  - 没有记录上一轮响应的 `response_id`，也没有发送 `previous_response_id`。
- **Pi 的做法** (`packages/ai/src/api/openai-codex-responses.ts:98, 576, 1490-1560`):
  - ChatGPT Codex 后端支持连接状态维系（Connection-scoped continuation）；
  - Pi 记录 `continuation.lastResponseId`；
  - 后续交互中，请求体发送：
    ```json
    {
      "previous_response_id": continuation.lastResponseId,
      "input": deltaItems
    }
    ```
    仅发送本轮新增的 delta 消息（如最新的 tool outputs），服务端自动将其嫁接到前序上下文中！
- **建议改法**：
  - 记录在“待 Yuki 决定”中：是否在 Codex 传输层引入连接级 `previous_response_id` 状态管理。
- **优先级**：`P1`
- **预期影响与验证**：Codex WebSocket 请求体体积下降 90% 以上，显著提升网络传输效率。

### 4.3 Reasoning 内容与双 ID 机制现状评价
- **nyan 表现优异点** (`src/services/provider/openai/responses.rs:208, 126-133, 380-389`):
  - nyan 在 PR 5b 中正确配置了 `"include": ["reasoning.encrypted_content"]`；
  - 通过 `ProviderState` 将完整的 `reasoning` item 跨轮无损回传给服务端；
  - 正确实现了 `call_id` 与 `fc_...` item id 的双 ID 复合拼接（`join_call_id` / `call_part` / `item_part`），并在此处与 Pi 保持了高度的一致性。
- **维持现状即可**。

---

## 5. Google Gemini 协议对齐审计

### 5.1 隐式缓存 (Implicit Caching) 机制与前缀触发条件
- **原理对标**：
  - Gemini API 的显式 `cachedContents` 接口需要单独调用创建接口、预付存储费用且需管理生命周期 TTL（通常最低阈值需 32k tokens）。
  - Pi 与主流实现均主要依赖 Google 的**隐式上下文缓存**（Implicit Prompt Caching）：只要输入内容超过 32k tokens，且**请求前缀（system_instruction + tools + 初始 contents）与之前完全一致**，Google 服务端会自动对相同前缀进行缓存，并在返回的 `usageMetadata.cachedContentTokenCount` 中上报命中数量（费用优惠约 75%）。
- **nyan 现状** (`src/services/provider/gemini/wire.rs:181-201`):
  - `build_system_instruction` 将 `request.system` 简单拼装。如果提示词中含有未收敛的动态环境信息，由于前缀 Token 变动，即使超过 32k tokens 也永远无法触发隐式缓存。
- **Pi 的做法** (`packages/ai/src/api/google-generative-ai.ts:232-252`):
  - 严格确保前缀文本确定性；
  - 准确解析 `cachedContentTokenCount` 并计算：
    `inputTokens = promptTokenCount - cachedContentTokenCount`。
- **建议改法**：
  - 确保系统指令与工具列表的绝对确定性，充分激活 Google 服务端的 32k+ 隐式缓存。
- **优先级**：`P0`

### 5.2 Gemini 3 系列思维控制参数不匹配 (`thinkingLevel` vs `thinkingBudget`)
- **nyan 现状** (`src/services/provider/gemini/wire.rs:129-134, 442-455`):
  ```rust
  match request.thinking {
      ThinkingPreference::Disabled => {}
      ThinkingPreference::Adaptive => {
          gen_config.thinking_config = Some(ThinkingConfig {
              thinking_budget: Some(-1),
          });
      }
      ThinkingPreference::Budget { tokens } => {
          let budget = tokens.map(|t| t as i32).unwrap_or(0);
          gen_config.thinking_config = Some(ThinkingConfig {
              thinking_budget: Some(budget),
          });
      }
  }
  ```
  - **严重协议缺陷**：nyan 目前只能输出 `thinking_budget`。
  - 对于 **Gemini 3 系列**（如 `gemini-3-pro`、`gemini-3-flash`、`gemini-3.8-flash` 等）以及未来的 Gemini 4，Google API 引入了离散的枚举值 **`thinkingLevel`**（`"MINIMAL"`、`"LOW"`、`"MEDIUM"`、`"HIGH"`），并弃用了 `-1` 这种动态 budget。
  - 传入 `thinking_budget: -1` 在新版模型上可能被服务端忽略或直接报 400（`Invalid thinking_budget for this model`）。
- **Pi 的做法** (`packages/ai/src/api/google-shared.ts:33-114`):
  - 编写了 `usesGoogleThinkingLevel(model)` 检测函数（匹配 `gemini-3*`, `gemini-4*`, `gemma-4*` 等）；
  - 当为 Gemini 3 系列时，发送 `thinking_config: { thinking_level: "HIGH" | "MEDIUM" | "LOW" }`；
  - 仅对旧版模型发送 `thinking_budget`。
- **建议改法**：
  - 在 `src/services/provider/gemini/wire.rs` 中为 `ThinkingConfig` 增加 `thinking_level` 字段；
  - 根据模型名或 profile capability 动态选择发送 `thinkingLevel` 还是 `thinkingBudget`。
- **优先级**：`P0`（直接影响 Gemini 3 / 3.8 当前主流推理模型的正常工作）
- **预期影响与验证**：使用 Gemini 3 模型时，思考参数生效且不再出现参数不兼容错误。

### 5.3 工具执行结果（ToolResult）中多模态图片丢失
- **nyan 现状** (`src/services/provider/gemini/wire.rs:310-320`):
  ```rust
  ToolResultContent::Parts(parts) => {
      let text = parts
          .iter()
          .filter_map(|p| match p {
              ToolResultPart::Text { text } => Some(text.as_str()),
              _ => None,
          })
          .collect::<Vec<_>>()
          .join("\n");
      serde_json::json!({ "output": text })
  }
  ```
  - **严重缺陷**：如果工具执行返回了图片（例如截图工具、`read` 读取本地 PNG、绘图工具返回等），nyan 的代码直接 `filter_map` 过滤掉非文本部分，**静默吞掉了所有图片**！Gemini 模型完全收不到图片数据。
- **Pi 的做法** (`packages/ai/src/api/transform-messages.ts`):
  - 在将消息传递给 Gemini 前，保留多模态信息，将图片作为 inlineData 合并到轮次中或在紧接着的 user message 中输入。
- **建议改法**：
  - 在 `gemini/wire.rs` 中，若工具返回包含图片，支持在 Gemini 的多模态响应结构中以 `inlineData` 发送，或者将图片提拔到随后的 `user` content part 中。
- **优先级**：`P1`（影响视觉工具调用的正确性）
- **预期影响与验证**：Gemini 能够正确“看见”工具返回的截图或图表。

---

## 6. 本地模型通用 OpenAI 兼容协议 (openaiChat)

### 6.1 KV Cache / Chunked Prefill 机制与前缀敏感性
- **底层特性**：
  - 本地推理引擎（vLLM、SGLang、llama.cpp、LMDeploy 等）全部依赖 Radix Attention 或 Prefix Caching；
  - 其缓存是基于 Token 维度的哈希树。**只要前缀中哪怕有 1 个字符发生变化（例如一次工作目录变动），整个哈希链从变动位置开始截断，后续全部重新计算 Prefill**。
  - 本地环境受限于 GPU 显存带宽，Prefill 阶段通常是延迟瓶颈（TTFT 可高达数秒甚至数十秒）。
- **优化建议**：
  - 针对 `openaiChat` 协议，严格保证 `system` 消息内容与 `tools` 数组序列化的逐字节一致性；
  - 动态信息（如 `cwd`）建议采用类似 Pi 的方式移至尾部，或者在本地模式下提供精简静态提示词开关。

### 6.2 本地推理模型 Reasoning Content 跨轮保留
- **现状与痛点**：
  - 本地部署的 DeepSeek-R1-Distill、Qwen-2.5-Coder、QwQ 等模型在多轮对话中，如果历史 assistant 消息丢失了 `<think>` 标签或 `reasoning_content`，模型在第 2 轮之后极易出现思考混乱、重复输出或者格式崩溃。
- **改法建议**：
  - 配合第 3.3 节的改动，在 `chat.rs` 中为本地模型打通 `reasoning_content` 的读取与历史回写通道。

---

## 7. 待 Yuki 决定事项 (Pending Decisions)

根据指令要求，保持 #42 的保守默认（不发 strict、max/xhigh 降到 high、reasoning.summary 固定 auto）不变，在此列出与 Pi 的差异及后续决策选项：

### 决策 1：Tool Schema 的 `strict` 模式策略
- **背景**：nyan 当前在 OpenAI Chat 中不发送 `strict`，在 Responses 中显式发送 `strict: false`。因为真实的复杂工具（带有 `anyOf`、可选字段、嵌套对象）无法直接通过 OpenAI 严格模式校验。
- **Pi 做法**：Pi 实现了 `makeStrictJsonSchema`（将所有可选字段补全为 `required` 并包裹 `anyOf: [T, null]`，补齐 `additionalProperties: false`），并通过捕获 `isStrictToolLimitErrorMessage` 在服务端超限时自动回退。
- **备选方案**：
  - **方案 A（维持现状，推荐先保持）**：继续保持保守默认，不发 strict 或发 `strict: false`，避免因 Schema 限制导致调用失败；
  - **方案 B（引入 Schema 严格化清洗器）**：移植 Pi 的 `makeStrictJsonSchema` 与编译超限自动 fallback 机制，在 profile 显式开启 `strict: true` 时启用。

### 决策 2：Reasoning Effort 的 `max` / `xhigh` 映射范围
- **背景**：OpenAI 官方目前仅定义了 `low`、`medium`、`high`。nyan 在 `src/services/provider/openai/mod.rs:216` 将 `max` 和 `xhigh` 统一 clamp 降级为 `high`。
- **Pi 做法**：Pi 同样对 OpenAI 将 `max`/`xhigh` clamp 到 `high`，仅在 Anthropic（Opus 4.6 / 4.7）保留 `max` 与 `xhigh`。
- **备选方案**：
  - **方案 A（维持现状，双方一致）**：非 Anthropic 协议继续强制 clamp 到 `high`；
  - **方案 B**：未来若第三方端点（如 xAI、OpenRouter 自定义档位）支持更高阶参数，改由模型元数据动态映射。

### 决策 3：Reasoning Summary 字段配置
- **背景**：nyan 在 OpenAI Responses 中将 `reasoning.summary` 硬编码为 `"auto"`。
- **Pi 做法**：Pi 默认也是 `"auto"`，但在调用参数中允许由 `options.reasoningSummary` 覆盖。
- **备选方案**：
  - **方案 A（维持现状）**：继续固定 `"auto"`；
  - **方案 B**：在 `ModelRequest` 或 profile 中暴露可选配置项。

### 决策 4：Gemini 3 离散 `thinkingLevel` 支持策略
- **背景**：Google 新一代模型（Gemini 3 / 3.8）弃用数字 `thinkingBudget`，改用 `thinkingLevel` 枚举（`MINIMAL` / `LOW` / `MEDIUM` / `HIGH`）。
- **备选方案**：
  - **方案 A（推荐采纳）**：在 Gemini adapter 中增加模型版本检测或 profile capability 声明，匹配到 Gemini 3+ 时切换发送 `thinkingLevel`；
  - **方案 B**：保持仅发送 `thinkingBudget`，等待 Google 官方弃用警告。

### 决策 5：Anthropic Message Threads Beta 机制引入
- **背景**：Pi 实现了 `message-threads-2026-08-12` 增量协议，通过 `previous_message_id` 发送增量消息。
- **备选方案**：
  - **方案 A（推荐）**：暂不引入 Message Threads。Anthropic 的 4 个 `cache_control` 滚动断点已经能够在全量消息下提供 95% 以上的缓存命中率，且全量消息无需处理服务端会话过期与并发锁；
  - **方案 B**：后续在独立 PR 中评估引入 Message Threads。

### 决策 6：OpenAI Codex Responses WebSocket 增量 `previous_response_id`
- **背景**：Pi 在 WebSocket 连接中支持通过 `previous_response_id` 仅发送增量 tool 输出。
- **备选方案**：
  - **方案 A（推荐）**：目前先完善 64 字节安全的 `prompt_cache_key`，保持全量发送；
  - **方案 B**：在后续长连接优化 PR 中实现 WebSocket 增量 session 状态管理。

---

## 8. 按协议拆分的实现 PR 建议列表

为降低代码冲突，并隔离核心通信逻辑，建议按协议分为 5 个独立 PR 进行演进：

| PR 序号 | 目标模块 / 协议 | 核心改动内容 | 优先级 | 影响的共享文件与冲突缓解措施 |
| :--- | :--- | :--- | :--- | :--- |
| **PR 1** | **Common IR & 提示词前缀稳定性** | 1. 统一 `Usage` 跨厂商缓存命中字段（增加 `cache_write_tokens` 等）；<br>2. 调整系统提示词结构，将高频变化的 `cwd` 等环境信息后置，保护静态前缀；<br>3. 修复 `openai/chat.rs` 对 DeepSeek/Kimi/OpenRouter 缓存命中字段解析。 | **P0** | **共享文件**：`src/services/provider/ir.rs`、`src/constants/prompts.rs`、`src/services/provider/openai/chat.rs`。<br>*缓解措施*：作为基石 PR 率先合入，后续各 adapter 依赖更新后的 IR 字段。 |
| **PR 2** | **Anthropic 协议对齐** | 1. 在最后一个工具上显式添加 `cache_control`；<br>2. 实现 `split_error_tool_result_images`，修复 `is_error: true` 带图引发的 400 崩溃；<br>3. 引入 `__pi_deferred_placeholder__` 保护动态工具前缀；<br>4. 支持 `PI_CACHE_RETENTION=long` 透传 1h TTL。 | **P0** | **共享文件**：`src/utils/api.rs`、`src/services/api/claude.rs`。<br>*缓解措施*：改动严格限制在 Anthropic 专有逻辑中，不影响其他 provider。 |
| **PR 3** | **OpenAI Chat Completions 协议对齐** | 1. 发送 64 字符截断安全的 `prompt_cache_key`；<br>2. 支持 `prompt_cache_retention: "24h"`；<br>3. 流式解析捕获 `reasoning_content`，并在多轮对话中支持推理内容保留；<br>4. 支持 OpenRouter 等网关的 Anthropic 风格 `cache_control`。 | **P0** | **共享文件**：`src/services/provider/openai/chat.rs`、`src/services/provider/openai/mod.rs`。<br>*缓解措施*：独立于 Responses，与 Responses 并行开发互不干扰。 |
| **PR 4** | **OpenAI Responses 协议对齐** | 1. 为 `Backend::Api` 补充发送 `prompt_cache_key`；<br>2. 针对 Codex 后端增加 `prompt_cache_key` 的 64 字符截断保护，防止超长 400 报错；<br>3. 允许配置 `prompt_cache_retention: "24h"`。 | **P1** | **共享文件**：`src/services/provider/openai/responses.rs`。<br>*缓解措施*：文件内改动集中在 `build_body`，无跨模块耦合。 |
| **PR 5** | **Google Gemini 协议对齐** | 1. 为 Gemini 3+ 模型增加离散 `thinkingLevel` 支持；<br>2. 修复 `ToolResult` 中丢失图片的问题，保留多模态数据；<br>3. 确保系统提示词与内容格式严格对齐 Google 32k+ 隐式缓存要求。 | **P0** | **共享文件**：`src/services/provider/gemini/wire.rs`、`src/services/provider/gemini/adapter.rs`。<br>*缓解措施*：Gemini 模块完全解耦，零冲突风险。 |

---

## 9. 验证与观测指南

在各 PR 实施后，可通过以下方式定量验证缓存命中率与协议正确性：

1. **Anthropic Messages 缓存验证**：
   - 观察请求流返回的 `StreamEventKind::MessageDelta` 中的 `Usage`；
   - 预期指标：首轮完成工具与系统加载后，第 2 轮对话起，`cache_read_input_tokens` 占总输入 Token 比例应达到 **85% ~ 98%**。
2. **OpenAI Chat / DeepSeek 缓存命中验证**：
   - 在 DeepSeek 接口下发起多轮代码审查；
   - 预期指标：第 2 轮起，返回的 usage 解析出非零的 `cache_read_tokens`（映射自 `prompt_cache_hit_tokens`），不再恒为 0。
3. **Gemini 隐式缓存验证**：
   - 输入一段超过 32k tokens 的大型代码库上下文；
   - 连续发起两次基于该上下文的问答；
   - 预期指标：第 2 次问答的 `Usage.cache_read_tokens` 应等于 `cachedContentTokenCount`，且网络首字延迟（TTFT）下降 50% 以上。
4. **错误图文边界断言验证**：
   - 执行单测：模拟工具返回 `is_error: true` 且携带 Base64 图片，断言 Anthropic adapter 输出的 wire 报文中，`tool_result` 内仅含提示文本，图片被放置在后续 user 消息中。
