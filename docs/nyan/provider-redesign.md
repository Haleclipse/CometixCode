# Provider / Model 抽象重设计

状态：设计稿（2026-11-03 修订），只读审计后产出，未实现。

本文只描述目标设计，不修改运行时实现。nyan 当前没有需要保护的用户配置，因此本设计直接替换旧模型/provider 配置，不提供兼容迁移层。唯一例外是第 1 个 PR 的临时 Anthropic 兼容层（决策 22），它只保证实施过程中请求可发，不读取旧字段，第 4 个 PR 删除。

## 0. 结论摘要

nyan 的目标是 provider 无关的通用 agent，而不是把 Anthropic API 当作运行时抽象。目标运行时分层如下：

```text
settings.nyan / CLI / session
        ↓
ProviderCatalog + ModelCatalog + CredentialResolver
        ↓
Provider-neutral Request IR
        ↓
Provider adapter
        ↓
normalized stream / usage / error / retry policy
```

已确定的核心原则：

1. 新配置统一放在 `settings.nyan.providers`、`settings.nyan.models`、`settings.nyan.selection`；旧模型/provider 字段直接删除，不做兼容迁移（PR 1 的临时 Anthropic 兼容层除外，见决策 22）。
2. 凡是选模型的地方都只认 profile：`--model`、`/model`、选择器、Agent 工具 `model` 参数、agent md 与 `selection.*`，写法统一为 profile 名 | `@light` | `@heavy` | `@main` | `inherit`，可带 `:effort`；不认识的报错。没有临时 `provider/model` 入口，也没有 `sonnet`/`opus`/`haiku`/`best`/`opusplan` 别名，新模型靠配置 profile。
3. 模型能力来自固定的、外部维护的模型元数据快照；不做运行时探测。profile 中显式能力覆盖元数据快照。
4. 不设默认 provider。没有可解析的 profile 时清晰报错或进入配置引导，绝不偷偷回退 Anthropic。唯一例外是 PR 1 的临时兼容层（决策 22），PR 4 删除。
5. Provider 协议由 adapter 隔离：Anthropic Messages、OpenAI Chat Completions、OpenAI Responses、Gemini；vLLM/Ollama 只走 OpenAI-compatible，不做 Ollama native adapter。
6. OAuth 由各 provider 自己管理，不做统一 broker。凭据 resolver 只提供 provider-scoped 的存取边界。
7. Project 与 local settings 只能引用 user/managed settings 已定义的 profile 名，不能新增 endpoint 或 credential file；引用不存在的 profile 直接报错。
8. Decision Provider 暂时只允许商业模型；本地 profile 即使支持分类能力，也不能绑定到 Decision Provider。
9. 主请求默认不跨 provider 自动重试；只有用户显式配置的 fallback 列表才允许切 provider。
10. Windows key file 必须通过 owner-only ACL 验证；无法验证就拒绝使用，并说明原因。

本文的接口是设计形状，不是要求一次性实现的代码 API。

## 一、现状审计

### 1.1 Settings、环境变量和启动覆盖

当前 `SettingsJson` 的普通模型字段是 `model`、`availableModels`、`modelOverrides`、`advisorModel`，另有 `env` 和 `httpHeaders`（`src/utils/settings/types.rs:60-85,122-124`）。nyan 自己的扩展集中在 `nyan` 顶层 key 下（`src/utils/settings/nyan.rs:1-19`）。目标配置会继续放在这个命名空间，但 `model`、`availableModels`、`modelOverrides` 等旧字段不再作为 provider 配置入口。

当前 settings 来源和合并顺序是 `userSettings → projectSettings → localSettings → cli flag → managed policy`；policy 最后覆盖前面的值（`src/utils/settings/constants.rs:89-105`、`src/utils/settings/mod.rs:246-300`）。新设计保留这个来源机制，但增加来源作用域约束：

- provider/profile 定义只来自 user 或 managed settings；
- project/local settings 只能写 profile 名、选择关系和不含 endpoint/credential 的策略；
- CLI `--model` 只接受 profile 引用，不落盘；
- policy 可以定义或限制 profile，并可限制 provider、模型、fallback 和本地 endpoint。

local settings 纳入限制是有意的扩展：它与 project settings 同属项目作用域，不能因为“通常不入库”就允许写 endpoint 或凭据文件。project 与 local 引用的 profile 名若在 user/managed settings 中不存在，启动时直接报错，不忽略、不回退。

### 1.2 当前模型/provider 选择链

当前主模型链是：

```text
CLI / bootstrap override
  → ANTHROPIC_MODEL
  → settings.model
  → Anthropic build-audience 默认模型
```

入口在 `src/main.rs:264-291`、`src/utils/model/model.rs:77-102,138-163`；`sonnet`、`opus`、`haiku`、`best`、`opusplan` 和 `[1m]` 是 Anthropic 专属别名（`src/utils/model/model.rs:104-135`）。当前 provider 由 `CLAUDE_CODE_USE_BEDROCK`、`CLAUDE_CODE_USE_VERTEX`、`CLAUDE_CODE_USE_FOUNDRY` 环境变量决定（`src/utils/model/providers.rs:7-27`），没有统一 provider trait。

这些旧入口只用于现状审计。目标实现中它们被删除：不再读取 `settings.model`、`ANTHROPIC_MODEL`、`GlobalConfig.modelContextWindows`，也不再保留 Anthropic 默认 provider。PR #59 已删除这些别名与 `model.rs` 中的对应代码，上文行号仅为历史记录。

### 1.3 Gateway model picker 与上下文窗口

当前 picker 将 `availableModels` 追加到内置 Sonnet/Opus/Haiku（`src/components/model_picker.rs:57-145`）；`additionalModelOptionsCache` 只提供 label/description。`modelOverrides` 将 Anthropic canonical ID 映射成 provider-specific ID（`src/utils/settings/types.rs:75-77`、`src/utils/model/model_strings.rs:63-103`）。PR #59 已将 picker 改为只列 profile，并删除内置 Sonnet/Opus/Haiku 与 `model_strings.rs`；此段仅为历史审计。

当前 `GlobalConfig.modelContextWindows` 是 gateway 专用旁路，按 model ID 查上下文长度（`src/utils/context.rs:126-154`，字段在 `src/utils/config.rs:617-621`）。目标设计删除这条旁路，统一使用固定模型元数据 + profile override；不存在“查不到时默认 200k”的隐式行为。

### 1.4 请求构造与 Anthropic 专有部分

当前请求链是：

```text
query loop
  → get_anthropic_client()
  → build_sdk_message_create_plan()
  → anthropic_sdk::BetaMessageCreateParams
  → provider-specific SDK / stream parser
```

client factory 构造 Direct、Bedrock、Foundry、Vertex 四种 Anthropic SDK client，并同时处理 header、API key、proxy、timeout 和 provider auth（`src/services/api/client.rs:1-9,152-163,184-246,291-515`）。目标设计中 Anthropic credential 只使用 API key。请求 body 直接使用 `BetaMessageCreateParams`，stream parser 直接处理 Anthropic content block delta（`src/services/api/claude.rs:2027-2116,2264-2403,3010-3175`）。

当前 header 已有可复用边界：

- common：`User-Agent`、`x-app`（`src/services/api/request_headers.rs:12-21`）；
- Anthropic adapter：session/container/remote header（`:23-50`）；
- `anthropic-beta` 只由 Anthropic 请求使用（`:52-58`）；
- `httpHeaders` 最后覆盖同名 header（`:60-82`）。

Anthropic 的 beta、cache control、Bedrock/Vertex/Foundry auth 和 Anthropic SDK 类型都必须留在 Anthropic adapter；Anthropic credential owner 只处理 API key，不实现 OAuth。其它 provider 的 OAuth flow、refresh、token schema 和副作用隔离仍由各自 owner 管理，不成为全局 provider 开关。

### 1.5 子 agent 和 Decision Provider

当前子 agent 选择优先级是 `CLAUDE_CODE_SUBAGENT_MODEL` → tool model → agent definition model → `inherit`（`src/utils/model/agent.rs:21-93`），并在 Bedrock 下继承 region prefix。目标设计删除这些旧 env/model alias 入口，改为 profile 名或父 agent 的 `ResolvedModel`。

当前 Decision Provider 用 `fast` / `accurate` 映射 EMRank 模型（`src/utils/settings/nyan.rs:838-902`、`src/services/decision/mod.rs:48-137`）。目标保留 tier 概念，但 tier 只能绑定 provider `kind` 属于商业集合的 profile（见决策 18）；本地 provider 不得进入 Decision Provider。

## 二、已定决策

以下 23 条是本设计的约束，不再作为待选项。1–12 为初始决策，13–23 为评审后新增：

1. **配置位置与替换**：新配置统一在 `settings.nyan.providers` / `models` / `selection`；直接替换旧配置。nyan 当前没有用户，因此不做兼容迁移、导入、deprecated 读取或旧字段警告。例外：PR 1 的临时 Anthropic 兼容层（决策 22）。
2. **模型引用**：只认 profile。`--model`、`/model`、选择器、Agent 工具 `model` 参数、agent md、`selection.*` 都只接受 profile 名 | `@light` | `@heavy` | `@main` | `inherit`（可带 `:effort`），不认识的报错：CLI 启动报错退出；`/model foo` 显示错误且不改任何状态；Agent 工具参数返回工具错误、不发请求。删除临时 `provider/model` 与 Claude Code 的 `sonnet`/`opus`/`haiku`/`best`/`opusplan`/`[1m]` 别名。
3. **能力元数据**：不做运行时探测；采用 Pi Agent 维护的固定模型元数据快照，profile 显式字段可覆盖快照。
4. **无默认 provider**：用户必须配置 provider/profile。没有可解析 profile 时给清晰错误或配置引导，不回退 Anthropic。
5. **OpenAI 两种协议**：Chat Completions 与 Responses API 在同一阶段实现，分别作为可声明的 adapter protocol。
6. **Ollama**：不实现 Ollama native adapter；Ollama 只能配置为 OpenAI-compatible endpoint。vLLM 同样走 OpenAI-compatible。
7. **OAuth**：各 provider 自己管理 OAuth、refresh、token schema 和持久化；不做统一 OAuth broker。Pi Agent、OpenCode、CLIProxyAPI 只作为设计参考，不作为运行时依赖。
8. **Project/local settings 边界**：project 与 local settings 绝对不能新增 endpoint 或 credential file，只能引用 user/managed settings 已定义的 profile 名。（local 纳入是有意的扩展，见 §1.1。）
9. **Decision Provider**：暂时只允许商业模型 profile，不能绑定本地 provider。商业判定的执行依据是决策 18（由 provider `kind` 派生，用户不可写）。
10. **旧字段删除**：直接删除 `settings.model`、`ANTHROPIC_MODEL`、`GlobalConfig.modelContextWindows` 的读取逻辑与配置入口；不保留兼容路径。
11. **跨 provider fallback**：默认禁止跨 provider 自动重试；只有用户显式配置的 fallback profile 列表允许切换 provider。
12. **Windows key file**：必须验证 owner-only ACL；无法验证就拒绝使用，并说明需要改用环境变量或系统凭据。
13. **元数据存放**：Pi 的模型元数据以快照形式提交进仓库（checked-in resource），构建和启动时都不下载、不联网。快照附 Pi commit、models.dev 来源版本、schema version 和内容 hash。
14. **快照更新**：每周更新一次。流程为「定时 CI 拉取上游 → 运行转换脚本 → 校验 → 有差异则开 PR」。无差异不开 PR；定时 CI 只开 PR，不自动合并。
15. **元数据覆盖与诊断**：profile 可以覆盖快照中的所有模型元数据字段（能力、context window、max output、displayName、cost 和 input limits 等）。被覆盖的字段在 `nyan doctor` 中列出，并标明值来自 profile override。
16. **首批 OAuth provider**：OpenAI（Codex）和 OpenCode。实现参考 Pi Agent（OpenAI Codex 的 PKCE + loopback / device code）、OpenCode（OpenCode Console 的 device flow）和 CLIProxyAPI（每个 provider 独立模块 + 按账号的文件存储）。Anthropic 只支持 API key；其它 OAuth provider 后置。
17. **凭据存储**：OAuth token 先存在 `~/.nyan` 下的 auth 文件中，权限仅本人可读（Unix 0600；Windows 验证 owner-only ACL，无法验证则拒绝使用）。系统 keychain（Windows Credential Manager、macOS Keychain、Linux secret service）后续再接。
18. **商业 / 本地判定**：Pi 没有该字段（它的 `compat` 按 baseUrl 自动识别，只用于 wire 兼容，不用于分类）。OpenCode 按 provider 插件身份分类：Ollama、LM Studio、vLLM 使用 `createLocalProviderPlugin`，分类不检查 host，也不由用户配置。nyan 照此落地：每个 provider 有内置 `kind`，`kind` 决定 `commercial`；用户不能在配置里写 `commercial`；未列入商业集合的 kind 一律视为非商业（失败关闭）。初始商业集合为 `anthropic`、`openai`、`gemini` 以及聚合网关 kind（包括 OpenRouter、中转等商业 gateway）。聚合网关即使上游模型属于本地或非商业，也按商业 provider 处理。
19. **主请求重试**：统一重试控制器默认最多重试 2 次（同 provider，且仅在未产生可见输出时，见 §7.2）；托管设置的全局上限默认是 2，硬上限是 5，profile 配置不能超过该全局上限。PR 8 前的过渡行为见决策 23。参考：OpenCode 会话请求最多重试 10 次、一次性调用 3 次（`packages/core/src/model-retry.ts:22-27`），nyan 的统一控制器有意收紧为上述上限。
20. **Gemini 单独 PR**：Gemini adapter 紧接 OpenAI 阶段之后单独一个 PR（PR 6），不与 OpenAI 并行。
21. **本地模型可进主 fallback**：vLLM / Ollama profile 可以显式写入 `selection.mainFallbacks`。本地 profile 永远不能进入 Decision Provider。
22. **临时 Anthropic 兼容层**：PR 1 保留一个临时的内部 Anthropic profile 兼容层，保证每个 PR 合入后 nyan 都能发请求。它是决策 4（无默认 provider）的唯一例外：只读取 Anthropic 现有凭据来源，不读取已删除的旧字段，不写入用户配置。PR 4（Anthropic adapter）落地时删除。**状态：已在 PR 4 删除。** Anthropic client 现在只从所选 profile 取 base URL 和 API key（`src/services/provider/anthropic/target.rs`、`credential.rs`），不再读取 `ANTHROPIC_API_KEY`、`ANTHROPIC_AUTH_TOKEN`、`ANTHROPIC_BASE_URL`、claude.ai OAuth token 或 `apiKeyHelper`；缺少 profile 或 key 时返回指明应配置项的错误，不回退。PR 4 只覆盖请求发送路径；认证/状态 UI 对 claude.ai 订阅状态的读取不在其中。
23. **过渡期重试行为**：PR 4 沿用现有 Anthropic 重试行为（`CLAUDE_CODE_MAX_RETRIES` 默认 10，529 上限 3，不改动）。PR 5（OpenAI）和 PR 6（Gemini）不新增自动重试，重试次数为 0，错误直接返回，直到 PR 8 统一重试控制器落地；PR 7 的本地 OpenAI-compatible 路径同样为 0。PR 8 把 Anthropic 默认从 10 降到 2，并启用托管全局硬上限 5，这是行为变更，必须在 PR 8 描述中写明。

## 三、目标抽象

### 3.1 Provider、profile 和模型引用

```rust
pub struct ProviderId(pub String);       // anthropic, openai, gemini, local-vllm
pub struct ModelProfileId(pub String);   // main, review, local-fast

pub struct ModelRef {
    pub profile: ModelProfileId,
    pub provider: ProviderId,
    pub model: String,                   // provider wire model id
}

pub struct ResolvedModel {
    pub reference: ModelRef,
    pub display_name: String,
    pub capabilities: ModelCapabilities,
    pub credential_profile: String,
    pub endpoint: Endpoint,
    pub commercial: bool,                // 由 provider kind 派生，不来自配置
}
```

持久化 `selection` 只写 profile 名。没有临时 `provider/model` 输入；project settings 不能定义 profile 内容。

`commercial` 不由用户写入配置，由 provider `kind` 决定（决策 18）：`kind` 是内置枚举，每个取值在代码中固定 commercial 属性；未列入商业集合的 kind 一律按非商业处理（失败关闭）。

### 3.2 能力模型

```rust
pub struct ModelCapabilities {
    pub context_window: u32,             // 必填才能运行
    pub max_output_tokens: u32,
    pub tools: ToolCapability,
    pub thinking: ThinkingCapability,
    pub images: ImageCapability,
    pub structured_output: bool,
    pub streaming: bool,
    pub prompt_cache: CacheCapability,
}
```

逐字段规则（"未声明"指快照和 profile 都没有该字段）：

| 字段 | 未声明时 |
|---|---|
| `context_window` | **必填**。缺失则 profile 解析失败，不能发请求 |
| `max_output_tokens` | **必填**。缺失则 profile 解析失败 |
| `tools` | 不支持（`none`），不发送 tool schema |
| `thinking` | 不支持（`none`），不发送 thinking 参数 |
| `images` | 不支持（`none`），图片 part 在 validate 阶段拒绝 |
| `structured_output` | `false` |
| `streaming` | `false`，按非流式请求处理 |
| `prompt_cache` | `none` |

未声明能力一律按不支持处理，不做请求 probe 猜测，也不默认为 true。快照中的值只作为默认，profile 显式字段可覆盖（见 §4.1）。

### 3.3 Pi Agent 模型元数据：来源、格式、许可证和更新

采用 [badlogic/pi-mono（当前仓库重定向到 earendil-works/pi）](https://github.com/badlogic/pi-mono) 的 `packages/ai` 模型目录作为首个外部元数据来源。Pi 仓库（canonical 为 earendil-works/pi，读取 SHA `1f8604f8`）为 MIT，版权行 `Copyright (c) 2025 Mario Zechner`。nyan 整体项目许可证为 AGPL-3.0-only；引入 MIT 的快照/必要来源材料不改变 nyan 的整体许可证，首批只在根目录 `NOTICE` 中保留 Pi 与 models.dev 的版权声明、许可证文本和来源 commit。nyan 不复制 Pi runtime，仅引入生成后的数据快照。

Pi 的通用模型数据主要来自 [models.dev](https://github.com/anomalyco/models.dev)，当前 Pi source adapter 读取 models.dev v2 experimental catalog，再叠加 Pi 自己的 provider/family 修正和手写模型定义。Pi 还合并了 OpenRouter、Vercel、NVIDIA、Radius 等来源；首批 NOTICE 不单独声明这些来源，未来实际引入其内容时再核实许可证和版权。因此“借用 Pi 元数据”不是把 models.dev 当作 nyan 的运行时 API。两者都是 MIT：models.dev 仓库 LICENSE 为 MIT（`Copyright (c) 2025 models.dev`，读取 SHA `9dfbad27`），未发现单独的数据许可证。Pi 快照派生自 models.dev 并叠加 Pi 自己的修正，因此首批 NOTICE 列出 Pi 与 models.dev 两家的版权声明。MIT 只覆盖代码和各自维护的内容，不覆盖模型厂商的服务条款和权重许可证；这些单独记录，不推断为 MIT。

Pi 的元数据结构可直接覆盖 nyan 的核心字段：

- `packages/ai/catalog/sources/models-dev.ts` 负责读取/归一化 models.dev 的 `modalities`、`capabilities.tools`、`capabilities.reasoning`、`limit.context/input/output`、`cost`、API/base URL 等字段；models.dev v2 schema 当前标为 experimental；
- `packages/ai/catalog/providers/*.ts` 负责 provider 的 API、base URL、compat、认证和 Pi-specific 修正；
- `packages/ai/src/providers/model-schema.ts` 定义生成数据 schema；
- `packages/ai/src/types.ts` 的 `BaseModel` 包含 `id`、`name`、`api`、`provider`、`baseUrl`、`input`、`inputLimits`、`cost`；
- chat `Model` 额外包含 `reasoning`、`thinkingLevelMap`、`promptCache`、`contextWindow`、`maxTokens` 和 API compatibility overrides；
- `packages/ai/src/providers/*.models.ts` 是 provider-specific source shards；`packages/ai/src/providers/data/*.json` 是生成/水合的数据；`models.generated.ts` 聚合 provider shards；
- `packages/ai/scripts/model-data.ts` 校验 schema version、provider 文件、manifest、structure hash 和每个生成文件的 SHA-256；
- `.github/workflows/publish-model-catalog.yml` 在 CI 成功后按工作日计划生成、校验并发布 catalog artifact。该 workflow 不是 nyan 的运行时依赖，nyan 只消费经过 pin 的快照。

Pi 还支持 provider 自己的 `refreshModels()` 动态目录和用户 `models.json` 覆盖；这不是能力探测。nyan 不启用运行时刷新，而是固定一个经过审查的生成快照。

引入方式：

1. 在 nyan 中新增一个**生成数据导入 PR**，记录 Pi commit、models.dev source commit/版本、model catalog schema version、生成时间，并在 `NOTICE` 中写入 Pi 与 models.dev 两家的 MIT 版权声明；不引入 Pi 的 TypeScript/Bun runtime。
2. 将 Pi 的 JSON model catalog 转换成 nyan 自己的稳定 JSON schema，保留 source commit、models.dev source 和原始 provider/model ID；生成 Rust 静态数据或 checked-in resource，不在启动时联网。
3. 做 schema/hash 校验：缺 `contextWindow`、`maxTokens`、`input`、`reasoning` 或 provider/api 的快照条目不作为可运行条目；profile 可以显式提供这些字段（决策 15）。上游新增字段默认忽略，schema breaking change 必须人工审查。
4. 更新走周更 PR（决策 14）：CI 升级 Pi/models.dev source commit，重新生成快照、跑校验、更新 NOTICE 和变更说明。运行时 nyan 不联网、不拉取远端元数据，上游变化只能通过合入的 PR 影响用户行为。
5. Pi catalog 的 `reasoning` 映射为 thinking 候选，`input` 中的 `image` 映射为图片能力，`contextWindow`/`maxTokens`/`promptCache`/`inputLimits` 映射为同名能力；`capabilities.tools`/`structured_output` 作为工具/结构化输出候选，但最终仍由 adapter protocol 和 profile override 决定，不宣称运行时已验证。
6. 不复制完整生成模型表到设计文档；实现时只提交必要快照和来源 manifest。模型厂商的商标、服务条款、权重许可证不随 Pi MIT 自动获得。

如果未来需要第二个元数据来源，必须先定义来源优先级、字段冲突、许可证和更新责任；不能把多个 catalog 在运行时无规则合并。

### 3.4 Provider protocol

| protocol | 覆盖 | 阶段 | 关键差异 |
|---|---|---|---|
| `anthropicMessages` | Anthropic API、Anthropic-compatible gateway、Bedrock/Vertex/Foundry 的 Anthropic wire 变体 | Anthropic adapter 阶段 | system blocks、thinking、tool schema、beta、cache control、SSE/专用 transport |
| `openaiChat` | OpenAI Chat Completions、OpenRouter/中转等商业聚合网关、vLLM、Ollama OpenAI-compatible endpoint | OpenAI 阶段 | `messages` content parts、tool calls、JSON response format、usage/finish reason |
| `openaiResponses` | OpenAI Responses API 与兼容 Responses endpoint | OpenAI 阶段，与 `openaiChat` 同 PR | `input` items、response output items、reasoning、background/continuation 语义 |
| `gemini` | Gemini API / Vertex Gemini | 后续独立阶段 | `contents`/`parts`、function call、thought parts、inline data |

不提供 `ollama` protocol。Ollama profile 必须声明 `protocol: openaiChat`，并把 Ollama 特有的 endpoint/header/body 差异限制在 OpenAI-compatible adapter 的显式 compatibility options 中；首版不实现 Ollama native API。

### 3.5 Provider-neutral Request IR

```rust
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    async fn send(
        &self,
        model: &ResolvedModel,
        request: &ModelRequest,
        cancel: &CancellationToken,
    ) -> Result<ModelStream, ProviderError>;

    fn validate(&self, model: &ResolvedModel, request: &ModelRequest)
        -> Result<(), CapabilityError>;
}
```

`ModelRequest` 包含消息、system blocks、tools、tool choice、thinking preference、图片、output limit 和 query source，但不包含 provider header/beta。adapter 负责 wire body、stream normalization、usage、stop reason、request id 和 provider-specific error mapping。

## 四、建议配置格式

### 4.1 Provider、profile 和 selection

```jsonc
{
  "nyan": {
    "providers": {
      "anthropic": {
        "kind": "anthropic",
        "protocol": "anthropicMessages",
        "baseUrl": "https://api.anthropic.com",
        "credential": { "env": "ANTHROPIC_API_KEY" }
      },
      "openai": {
        "kind": "openai",
        "protocols": ["openaiChat", "openaiResponses"],
        "baseUrl": "https://api.openai.com/v1",
        "credential": { "env": "OPENAI_API_KEY" }
      },
      "local-vllm": {
        "kind": "vllm",
        "protocol": "openaiChat",
        "baseUrl": "http://127.0.0.1:8000/v1",
        "credential": { "optionalEnv": "VLLM_API_KEY" }
      },
      "local-ollama": {
        "kind": "ollama",
        "protocol": "openaiChat",
        "baseUrl": "http://127.0.0.1:11434/v1",
        "credential": { "none": true }
      }
    },
    "models": {
      "main": {
        "provider": "anthropic",
        "model": "claude-sonnet-4-6",
        "displayName": "Sonnet",
        "capabilities": {
          "contextWindow": 200000,
          "maxOutputTokens": 128000,
          "tools": "native",
          "thinking": "adaptive",
          "images": "input",
          "structuredOutput": true,
          "streaming": true,
          "promptCache": "native"
        }
      },
      "local-fast": {
        "provider": "local-vllm",
        "model": "Qwen/Qwen3-32B",
        "capabilities": {
          "contextWindow": 131072,
          "maxOutputTokens": 16384,
          "tools": "native",
          "thinking": "budgeted",
          "images": "none",
          "structuredOutput": true,
          "streaming": true,
          "promptCache": "none"
        }
      },
      "commercial-fast": {
        "provider": "anthropic",
        "model": "claude-haiku-4-5-20251001",
        "displayName": "Haiku (classifier)",
        "capabilities": { "structuredOutput": true }
      },
      "commercial-accurate": {
        "provider": "anthropic",
        "model": "claude-sonnet-4-6",
        "displayName": "Sonnet (classifier)",
        "capabilities": { "structuredOutput": true }
      }
    },
    "selection": {
      "main": "main",
      "mainFallbacks": [],
      "subagent": "inherit",
      "subagents": {
        "explore": "main",
        "plan": "main"
      },
      "decision": {
        "fast": "commercial-fast",
        "accurate": "commercial-accurate"
      }
    }
  }
}
```

规则：

- `selection.main` 是必填的 profile 名；未配置或 profile 不存在时，启动进入配置引导/返回明确错误；不使用内置 Anthropic default。
- `mainFallbacks` 是用户显式允许的 fallback profile 列表，默认空；不配置就不跨 provider 自动切换。
- `selection.subagent` 和 `subagents.*` 只能是 profile 名或 `inherit`；project/local settings 可以引用 user/managed 中存在的名字，但不能定义 provider/profile 内容。引用的名字不存在时直接报错（§1.1）。
- profile 可以覆盖 Pi snapshot 的全部模型元数据字段；覆盖值必须经 schema 校验，不能把未声明能力自动设为 true。被覆盖的字段在 `nyan doctor` 中列出（决策 15）。
- profile 定义只能写在 user 或 managed settings。project/local settings 只能写 profile 名、选择关系和 policy，不得写 `baseUrl`、`credential.file`、secret、OAuth client data 或任意新的 provider ID。
- `commercial` 不是配置字段。provider 的 `kind` 决定商业属性（决策 18），用户写入 `commercial` 会被 schema 拒绝。
- `kind` 是内置枚举（如 `anthropic`、`openai`、`gemini`、`vllm`、`ollama`，以及 OpenRouter/中转等聚合网关 kind）。未知 `kind` 在 schema 阶段报错，不作为自定义 provider 处理。聚合网关 kind 属于商业集合。
- `mainFallbacks` 可以包含本地 profile（决策 21）；Decision Provider 的 tier 不能包含。

### 4.2 Credential 引用与 OAuth owner

```jsonc
{
  "credential": { "env": "OPENAI_API_KEY" },
  "credential": { "file": "~/.nyan/keys/openai" },
  "credential": { "optionalEnv": "VLLM_API_KEY" },
  "credential": { "oauth": "opencode" },
  "credential": { "oauth": "openai-codex" }
}
```

credential reference 只引用来源，不把 secret 写入 settings：

- `env`：读取指定变量；未设置是 `missingCredential`；
- `file`：单行读取；Unix 要求 0600；Windows 必须验证 owner-only ACL，无法验证立即拒绝并说明需要 env 或系统凭据；
- `optionalEnv`：允许无认证本地 endpoint（仅限本地 kind `vllm` / `ollama`；商业 kind 在 settings 校验与凭据解析阶段拒绝），收到 401 仍报告 auth error；
- `oauth`：绑定一个 provider-owned OAuth profile，flow、refresh、token schema、存储和删除都由该 provider owner 实现。

**Pi Agent 参考**：

- `packages/ai/src/auth/oauth/load.ts` 明确按 provider 注册 OAuth loaders：OpenAI Codex、OpenAI ChatGPT、GitHub Copilot、OpenRouter、Kimi Coding、Meta、xAI，以及可参数化的 Radius；每个 flow 是独立模块，不是统一 token broker。
- `packages/coding-agent/src/core/auth-storage.ts` 使用 `auth.json` 文件存储，以 provider id 为 key；OAuth credential 包含 access、refresh、expires，API key 支持 env indirection；写入使用锁，目录默认 0700、文件创建 mode 0600，并处理 Windows 删除 pending 的锁重试。
- Pi 的 OAuth module 使用 PKCE、loopback callback 或手工粘贴 redirect/code；provider-specific endpoint 和 scope 位于各自的 OAuth module。

**CLIProxyAPI 参考**：

- [router-for-me/CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) 的 `internal/auth/` 按 provider 拆分 Claude、Codex/OpenAI、Gemini/Vertex/Antigravity、Kimi、Devin、Meta、xAI 等 auth/refresh/token 实现；README 也把 Claude Code、Codex、Gemini、Grok、Kimi、Devin 等列为独立登录/路由能力。
- `sdk/auth/filestore.go` 的 `FileTokenStore` 遍历配置的 auth directory，保存 provider-specific JSON；创建目录使用 0700，文件使用 0600；每个 provider 通过自己的 `TokenStorage` 序列化和刷新。
- `internal/auth/claude/filename.go`、`internal/auth/codex/filename.go` 按 provider/account identity 生成文件名，保留 legacy filename fallback；这说明多账户隔离应在 provider owner 中完成，而不是由统一 broker 猜 token 格式。
- CLIProxyAPI 是 Go gateway/proxy，不作为 nyan 的运行时依赖；nyan 只借鉴“provider-specific OAuth + per-account file store + refresh owner”的边界。

首批 NOTICE 只保留 Pi 与 models.dev 的版权声明和许可证材料；OpenCode、CLIProxyAPI 仅作设计参考，nyan 不复制其实现或数据。若未来实际引入其它来源，再单独核实许可证并更新 NOTICE。

**OpenCode 参考（认证方式）**：
- `opencode auth login` 的方法类型为 key、command、external、oauth（`packages/cli/src/commands/handlers/auth/login.ts:40-56`）。
- 凭据存于 SQLite 的 `credential` 表，`value` 为明文 JSON，未加密；POSIX 上数据库文件 chmod 0600，Windows 依赖 profile ACL（`packages/core/src/database/database.ts:83-96`）。nyan 不照搬 SQLite，只借用“按类型存储、权限收紧”的思路。
- OpenAI 的 "Sign in with ChatGPT" 为 PKCE + 127.0.0.1 回调 + 动态客户端注册（`packages/core/src/plugin/provider/chatgpt.ts:21-27,100-151`）。
- OpenCode Console 为 device flow，client ID `opencode-cli`（`packages/core/src/plugin/provider/opencode.ts:24-25,93-99`）。
- Anthropic 在 nyan 中只支持 `ANTHROPIC_API_KEY`；不加入 OAuth flow，也不把外部 `claude auth login` 当作 nyan 的 credential owner。

**首批 OAuth 范围（决策 16）**：OpenAI Codex（Pi 的 PKCE 或 device code）和 OpenCode（device flow）。每个 flow 为独立模块，token schema、refresh 和存储由 provider owner 实现；Anthropic 只支持 API key。

**存储（决策 17）**：先写入 `~/.nyan/auth/<provider>.json`，目录 0700、文件 0600；参考 Pi 的做法（`auth-storage.ts:37,71,77`）使用文件锁串行化读改写。Windows 必须验证 owner-only ACL，无法验证则拒绝使用。系统 keychain 后续再接。

**PR 3 实现（`src/utils/provider_auth/`）**：

- `reference.rs`：`env` / `optionalEnv` / `file` 的读取与错误；`file` 要求 owner-only（Unix 0600、拒绝符号链接；Windows 校验 ACL）且只有一行。`Secret` 的 `Debug` 不输出内容。
- `store.rs`：`~/.nyan/auth/<provider>.json`，每个 provider 一个文件、文件内按 account 分条目（`version: 1`）。provider id 只允许 `a-z0-9-`（它是文件名）。读无锁（文件原子替换），写必须持有 `<provider>.lock` 的独占锁（`AuthLock`）。路径不接受环境变量覆盖，避免 settings 注入改变 token 位置。
- `refresh.rs`：`fresh_credential` 在 provider 锁内重读后才刷新，等锁的调用方直接拿到先到者写入的新 token，不会重复消耗一次性 refresh token。不同 provider 互不阻塞。
- `oauth/openai_codex.rs`（PKCE + 授权码）、`oauth/opencode.rs`（device flow）：各自持有 endpoint、client id、token 解析与 refresh；公共部分只有表单/JSON 提交和“错误里不引用响应体”的失败分类。token 请求不跟随重定向，OpenCode 的 server 只接受 https 或回环 http。
- `resolve.rs`：`validate_reference` 按 provider `kind` 限定 `oauth` 可用的模块（`openai` ↔ `openai-codex`，`gateway` ↔ `opencode`），`anthropic` 配 `oauth` 一律拒绝；限定 `optionalEnv` 仅允许本地 kind（`vllm`、`ollama`），商业 kind 配 `optionalEnv` 一律拒绝。
- 新存储不读取 `OAUTH_CREDENTIAL_SIDE_EFFECTS_ENABLED`；该开关仍只保护继承下来的 Anthropic / MCP OAuth 出口；provider 凭据不经过这些出口。这些旧出口的删除范围见 PR 3 描述的“待决策”。
- PR 3 不发真实模型请求、不开浏览器、不监听端口、不提供登录命令；登录步骤以函数形式暴露，由后续 provider adapter 阶段驱动。

## 五、模型选择协议与思考深度

### 5.1 模型选择与档位

会话的模型就是会话 profile 的模型；只有 profile 能选：

```text
CLI --model <ref>[:effort]
  > session /model <ref>[:effort] 或选择器回车（只改当前 session）
  > resume 还原的会话 profile
  > settings.nyan.selection.main（profile 名，必填）
  > 无配置：明确报错 / 选择器只显示配置引导，绝不隐式回退 Anthropic
```

切换会话 profile 后，请求走该 profile 自己的 provider、凭据与协议。

- **档位定义**：`selection.main`、`selection.light`、`selection.heavy` 均为 profile 名；`light`/`heavy` 未显式配置时自动回落 `main`。
- **统一写法 `ref[:effort]`**：`selection.subagents.<type>`、`selection.utility.<role>`、agent md 的 `model:`、Agent 工具调用参数 `model` 均统一采用 `ref[:effort]` 格式。
  - `ref` = profile 名 | `@light` | `@heavy` | `@main` | `inherit`；其他任何值都是错误。
  - 只有 `:effort` 时（如 `:high`）等同于 `inherit:high`。按最后一个 `:` 拆分，且只有后缀属于合法 effort 档位时才拆分（兼容 `qwen3:32b` 这类含冒号的 profile 名）。
- **skill 不选模型**（照 Pi）：skill / plugin command 的 `model:`、`effort:` frontmatter 被忽略；需要特定模型就用带 `model:` 的 agent。理由：inline skill 中途换 profile 会让跨模型缓存前缀失效（Opus 5.5、10 万前缀约 $0.80/次）。
- **默认规则**：
  - 子 agent 类型：`general-purpose`、`plan` = `inherit`；`explore` = `@light`；`review` = `@heavy`。
  - Utility 角色（`title`、`summary`、`commit_message`、`webfetch` 摘要、`bash_prefix` 提取、`prompt_suggestions`、`memory` 提取、`side_query`）= `@light`。
  - `compaction`、权限分类器（`classifier`）= `@main`。
- **未知 profile 校验**：引用未知的 profile 名在启动时报错（`selection.*` 校验）；`--model` 不认识时启动报错；Agent 工具调用参数不认识时返回工具错误、不发请求；agent md 的 `model:` 不认识时该 agent 运行报错（加载时校验归 /agents PR）；`@tier`（`@light` / `@heavy` / `@main`）与 `inherit` 属于保留档位/关键字，永不报未知 profile 错误。`@tier` 在没有可指的 profile 时（未配置 `selection.main`）跟随父会话，与 `inherit` 相同。
- **子 agent 模型解析链**：
  ```text
  调用参数 model (ref[:effort])
    > agent md 的 model: 字段 (ref[:effort])
    > selection.subagents.<type> (ref[:effort])
    > selection.subagent (ref[:effort])
    > 类型默认值 (explore=@light, review=@heavy, general-purpose/plan 及其他类型=inherit)
  ```
  `inherit` 取父会话**当前**的模型、effort 与 profile（含 `/model` 的会话选择）。决定模型的那一层写了不认识的 ref 即报错，不回落到下一层。解析结果同时给出模型、effort 和 profile；请求带着该 profile 走自己的 provider、凭据与协议。`inherit` 时整个 profile 跟随父会话，指定 profile 或 `@tier` 时换用该 profile。
- **删除 CC 遗留别名**：彻底删除 `haiku`/`sonnet`/`opus`/`best`/`opusplan`/`[1m]` 等硬编码别名、`ANTHROPIC_DEFAULT_*_MODEL` 默认模型、`opusplan` 的 plan 模式切换和 Anthropic 专属 tier 匹配（`alias_matches_parent_tier`、Bedrock 前缀继承等）。
- **选择器（`/model` 无参数或 Alt+P）**：只列 profile，当前会话的 profile 第一，默认（`selection.main`）第二，其余按名字排序；每行显示 profile 名与模型 ID，默认 profile 标注 `default`。←/→ 调 effort；回车只改当前会话（切换会话 profile 与 effort）；Ctrl+S 设为默认：写用户 settings 的 `nyan.selection.main` 与该 profile 的 `effort`，若 project/local settings 也设了 `selection.main` 且不同，提示“This project still uses X”。没有 profile 时只显示配置引导（`settings.nyan.models` / `selection.main`）。没有“全部模型”页签。
- **删除**：Fast mode 全套（`/fast`、Alt+O、设置项、徽标）、Alt+T 思考开关（关思考用 effort `off`）、独立的 effort 选择器（`/effort <level>` 直设保留，`/effort` 无参数只打印当前 effort 与可选值）。

### 5.2 统一思考深度 Effort

- **统一四档**：统一 effort 只有四档：`off < medium < high < max`。`off` 表示不思考（把原 `ThinkingPreference` 开关完全并入）。
- **Profile 配置**：Profile 可配置 `effort`，未配置时使用模型默认行为。
- **决定模型层决定 Effort**：Effort 严格跟着“决定模型的那一层”走：
  - 该层显式写了 `:effort` 则使用该 effort；
  - 否则，若该层是 `inherit` → 继承父会话当前 effort；
  - 若指定 profile 或 `@tier` → 使用该 profile 配置的 `effort`（无配置则模型默认）；
  - 不单独沿解析链跨层合并 effort，确保父会话的思考深度不会意外落到其它厂商模型上。
- **各 Adapter 映射与静默就近夹取**：
  - **Anthropic**：支持 `output_config.effort` 的模型（Opus 4.5+、Sonnet 4.6+ 等）发送 `medium` / `high` / `max`（Opus 4.5 没有 `max`，夹取为 `high`）；`off` 关闭 thinking 且不发送该参数。不支持该参数的旧模型（Claude 3.x、Sonnet 4/4.5、Opus 4/4.1、Haiku 4.5）不发送，改按档位设 thinking budget（`medium` 8192、`high` 24576、`max` 取模型上限）。支持 adaptive thinking 的模型由 effort 档位承载深度，不再设 budget。
  - **OpenAI**：映射为 `reasoning_effort` / `reasoning.effort`，`medium` / `high` 直通；`max` 在支持 `xhigh` 的模型（GPT-5.2+、GPT-6 系列）发送 `xhigh`，其余夹取为 `high`；`off` 取该模型族接受的最低档：GPT-5.1 / 5.2+ / GPT-6 Sol 为 `none`，GPT-5 为 `minimal`，GPT-5 Codex 与 o 系列为 `low`，GPT-6.1 Sol 与 GPT-6 Astra（拒绝 `none` / `minimal`）为 `low`；无法确认接受任何关闭值的模型（兼容网关、本地服务器）不发送字段。
  - **Gemini**：Gemini 3 映射为 `thinkingLevel`（`MEDIUM` / `HIGH`），`off` 夹取为所有 Gemini 3 文本模型都支持的 `LOW`；Gemini 2.5 按档位推算 `thinkingBudget`（`medium` 8192、`high` 24576、`max` 为动态预算 `-1`，`off` 为 0，Pro 不接受 0，夹取为下限 128）。`off` 不请求思考摘要。
  - **DeepSeek / Kimi**：只有开/关（`off` 关，其余开）。目标行为如此；本 PR 暂走 OpenAI 兼容路径发送 `reasoning_effort`（DeepSeek 把 `medium` 归并为 `high`），`off` 不发送字段、保持厂商默认。显式的 `thinking.type` 开关因各模型字段不同（如 DeepSeek 与 Kimi k2.6 / k3），留待能对真实账号验证后再做，当前可用 profile `extra` 补写。
  - **不支持推理的模型（含本地模型）**：profile 未声明 thinking 能力时不发送任何推理字段，静默不报错。
- **会话作用域与持久化**：
  - `/effort` 命令只改当前会话。
  - `/model` 选择器中通过左右键调节深度，回车确认只修改当前会话；Ctrl+S（设为默认）将 profile 写入用户 settings 的 `selection.main` 并持久化该 profile 的 `effort`。
  - **切换 profile 时的 effort**：`/model <profile>:effort` 或在选择器里调过 → 用该值；否则用新 profile 配置的 `effort`；再否则模型默认。绝不沿用切换前的会话 effort。
  - 会话内的 profile 与 effort 变化记录为会话事件（`model-change` 存 profile 名与该次切换定下的 effort，`effort-change` 存 effort），resume 时按顺序还原；记录的 profile 已不存在时回到配置的 `selection.main`（用它自己的 effort），并显示提示。
  - 删除全局 `effortLevel` 设置、`CLAUDE_CODE_EFFORT_LEVEL` 环境变量、launch pinning、org-limit clamp、ultracode 等 Claude Code 遗留逻辑。
  - Agent md 文件不再包含独立的 `effort:` 字段；Agent 工具调用参数也不再提供独立的 `effort` 参数。

### 5.3 Profile 其余参数与继承

- **字段精简（借鉴 Pi Agent）**：Profile 在现有字段（`contextWindow`、`maxOutputTokens`、`capabilities`、`retry`、`cacheRetention` 等）之外，仅扩展：
  - `effort`: 可选统一深度（`off` / `medium` / `high` / `max`）。
  - `extra`: 可选原样 JSON 对象（`Option<serde_json::Value>`），请求时最后合并进底层请求体。
- `cacheRetention`：`long`（默认）/ `short` / `none`，是缓存时长的唯一来源（照 Pi）：
  - Anthropic：`long` 在每个断点发 `cache_control.ttl: "1h"`，`short` 发 `"5m"`（显式，避免网关把无时长标记升成 1h），`none` 不打断点。
  - OpenAI（仅 `openai` kind）：`long` 时，目录 cache 写入价 > 0 的模型（GPT-5.6 起）在 Responses 上发 `prompt_cache_options: {"ttl": "30m"}`，更早的模型发 `prompt_cache_retention: "24h"`；Chat 一律发 `"24h"`。ChatGPT 后端、网关、本地服务不发。
  - DeepSeek、Gemini：自动缓存，不发任何缓存参数。
  - 默认 `long` 的依据：Opus 5.5、10 万 token 前缀，1h cache 过期重写约 $0.80，续命读取约 $0.02。
- 不添加 `temperature` / `top_p` / `stop` / `thinking_budget` 等冗余 profile 字段。
- 子 agent 选择 `inherit` 时，整个 profile 完整跟随父会话当前生效的 profile（或临时配置）。

### 5.4 Decision Provider

`fast` / `accurate` 是逻辑 tier，只允许选择 provider `kind` 属于商业集合的 profile（决策 9、18）：

```jsonc
{
  "nyan": {
    "selection": {
      "decision": {
        "fast": "commercial-fast",
        "accurate": "commercial-accurate"
      }
    }
  }
}
```

约束：

1. profile 必须显式声明 `structuredOutput: true`（未声明按不支持，见 §3.2），并有足够的 context window；provider `kind` 必须属于商业集合；
2. local-vLLM、Ollama、其它非商业 kind 的 profile 在 schema/resolver 阶段拒绝；
3. Decision Provider 的 fallback 也只能来自显式 commercial fallback 列表；
4. down/timeout/low-confidence 仍回到确定性路径，不切到主 agent profile；
5. 保留现有 `off/shadow/active` 语义和 credential redaction，不改变硬安全规则。

## 六、上下文窗口、token 计算与缺失元数据

### 6.1 没有声明 context window 时

`contextWindow` 是可运行 profile 的必填能力：

- Pi 快照有 `contextWindow` 时直接导入；
- profile 显式 override 时使用 override；
- 两者都没有时，profile 解析失败，不能发送请求；
- 不使用旧的 `modelContextWindows`、200k 默认值、模型名称猜测或 runtime probe。

这样做会让缺元数据的 profile 立即暴露配置错误，而不是在长对话中随机 overflow。

### 6.2 token 计数规则

即使 profile 无法运行，诊断仍可显示估算 token：

1. 预请求使用 provider-neutral 的现有 rough estimator，按文本、tool schema、图片引用和消息结构估算；它用于 UI/诊断，不声称与 provider tokenizer 精确一致；
2. 发送前用 `contextWindow - reservedOutputTokens - safetyMargin` 做 admission；任何无法计算的图片/二进制 part 按 profile 的 `inputLimits` 或保守上限计入；
3. provider 返回的 usage 是事后权威值，按 provider adapter 归一化记录；不回写成新的能力元数据；
4. contextWindow 缺失时没有 admission threshold，也不启动自动 compaction 试探；先报 profile invalid；
5. `maxOutputTokens` 取 profile/元数据与 provider adapter 上限的较小值，不能因未知而使用无限或旧 Anthropic 默认。

### 6.3 Cache 与 token 统计

`promptCache` 只描述已知的 provider cache 语义；未知时按 none。cache 命中、写入和读取 token 只在 provider 返回 usage 后记录，不用猜测或跨 provider 迁移 cache receipt。

## 七、限流、错误和重试语义

### 7.1 统一错误模型

adapter 把 provider 错误归一化为：

```text
rateLimited { retryAfter, providerWindow? }
overloaded
transient { status, retryable }
contextOverflow
invalidRequest
unauthorized
forbidden
notFound
transport
partialStream
```

原始 status、Retry-After 和 provider-specific rate-limit headers 只进入安全的 request receipt；不把 token 或完整响应写日志。

### 7.2 默认重试规则

- 同一 provider 的**请求尚未产生可见输出**时，才允许对明确的 transient/rate-limited/overloaded 错误重试；统一控制器默认最多 2 次（决策 19），指数退避 + jitter，并受 `maxRetryDelay` 限制。托管设置可设全局上限，但不得超过硬上限 5；profile 不能超过该全局上限；
- 过渡期行为见决策 23：PR 4 沿用现有 Anthropic 默认 10 次，PR 5–7 为 0 次，PR 8 统一为默认 2 次并启用硬上限 5；
- 优先尊重标准 `Retry-After`；provider adapter 可解析自己的 reset headers，但不能把某家 header 当成通用协议；
- 一旦流已经产生 normalized output，连接失败返回 `partialStream`，不自动重放整个请求，避免重复工具调用和重复副作用；
- 400/401/403/404、能力不支持、上下文溢出和 schema 错误不重试；
- provider 选择不因 429/5xx 自动改变。跨 provider 只有在用户显式提供 `mainFallbacks`/对应 tier fallback 列表时才允许，并记录 provider switch；
- fallback 列表按顺序尝试，每个 profile 使用自己的 credential、协议、rate-limit policy 和 capability snapshot；列表为空就是不 fallback。

### 7.3 各协议语义

| provider/protocol | 限流/暂态信号 | 默认处理 |
|---|---|---|
| Anthropic Messages | HTTP 429、529 overloaded、408/5xx；优先 `Retry-After` 与 Anthropic rate-limit headers | 未输出前最多 2 次同 provider retry；429/529 不自动切其它 provider |
| OpenAI Chat Completions | HTTP 429、408/5xx；优先 `Retry-After`，可记录 `x-ratelimit-*` reset 信息 | 与 Responses 共用 retry controller，但 adapter 分别解析 body/error；流已输出则 partialStream |
| OpenAI Responses | HTTP 429、408/5xx，以及 Responses-specific error body | 与 Chat 同阶段实现；不得把 Responses continuation 当成可安全重放的完整请求，除非尚未产生输出 |
| Gemini | 429/503 或 provider 明确标为 `RESOURCE_EXHAUSTED`/暂态的响应；优先标准 retry hint | adapter 负责把 Google error reason 映射为 rateLimited/overloaded；无 hint 时使用受限指数退避 |
| OpenAI-compatible vLLM/Ollama | 只依赖标准 HTTP 429/408/5xx；不假设某个 gateway header | profile 默认可关闭 retry；开启后只在同 endpoint 重试，跨 provider 仍需显式 fallback |

“各家 provider 支持什么 header/错误码”属于 adapter contract 和测试 fixture，不在公共层硬编码。provider 更新 rate-limit 语义时更新 adapter 与 fixture，不修改用户配置含义。

### 7.4 PR 5 实现：OpenAI adapters（`src/services/provider/openai/`）

**协议选择。** profile 用 `protocol` 声明自己说哪一种；provider 同时声明 `openaiChat` 和 `openaiResponses` 时必须写，否则解析报 `AmbiguousProtocol`。解析结果只有一个协议，之后各层都读这个值：Chat adapter 的 `validate` 拒绝 Responses profile，反之亦然；Anthropic 的 target 拒绝非 Anthropic 协议的 profile。ProviderState 带协议标签，只被产生它的协议回放。

**重试。** 决策 23：PR 5 重试次数为 0（主请求路径对非 Anthropic profile 把 `max_retries` 设为 0，且不走 Anthropic 的 non-streaming fallback）。PR 5 只做到：错误分类（含 `Retry-After` / `retry-after-ms` 解析）、流已产出内容后的失败归为 `PartialStream`。§九 PR 5 行“未输出重试”由 PR 8 的统一控制器落地，不在 PR 5。

**请求映射（IR → wire）。**

| IR | Chat Completions | Responses |
|---|---|---|
| `system` | 首条 `role: system`（去掉缓存边界标记） | `instructions`；ChatGPT 后端永不为空（缺省 `You are a helpful assistant.`） |
| `UserPart::Text` | `content` 字符串；含图片时为 `text` / `image_url` 数组 | `message` 的 `input_text` |
| `UserPart::Image` | `image_url`（data URL 或 URL） | `input_image`（`detail: auto`） |
| `UserPart::Document` | `file`（`file_data` data URL） | `input_file` |
| `AssistantPart::Text` | `assistant.content` | `message` / `output_text`（`status: completed`） |
| `AssistantPart::ToolCall` | `tool_calls[]`，`id` 取 `call_id` 部分、限 40 字符、超长加哈希后缀 | `function_call`，`call_id`；`id`（`fc_…`，≤64）只对 API 后端且格式合法时带上 |
| `UserPart::ToolResult` | `role: tool`；其中图片放在紧随的 user 消息里 | `function_call_output`；图片留在 `output` 数组内 |
| `Thinking` / `RedactedThinking` | 丢弃（Chat 没有输入通道） | 仅本协议的 ProviderState：整条 reasoning item 原样回放 |
| `ServerToolCall` / `ServerToolResult` | 丢弃（Anthropic 概念） | 丢弃 |
| `ToolSpec` | `tools[].function`，不带 `strict`；`ToolSearch` 不发送 | `tools[]`，显式 `strict: false`；`ToolSearch` 不发送 |
| `tool_choice` / `disable_parallel_tools` | `tool_choice` / `parallel_tool_calls: false` | 同名 |
| `thinking` + `effort` | `reasoning_effort`（`max`/`xhigh` 发 `high`；token 预算按档位折算；Adaptive 无 effort 不发） | `reasoning: {effort, summary: auto}` |
| `structured_output` | `response_format: json_schema`（`strict: false`） | `text.format: json_schema`（`strict: false`） |
| `max_output_tokens` | `max_completion_tokens`，取 profile 上限的较小值 | `max_output_tokens`（≥16）；ChatGPT 后端不发 |
| `temperature` | 仅在 profile 未声明 thinking 时发送 | 同左；ChatGPT 后端不发 |
| 固定字段 | `stream`、`stream_options.include_usage` | `stream`、`store: false`、`include: [reasoning.encrypted_content]` |

未声明的能力按“不支持”处理（§3.2）：tools / thinking 不支持时从请求中省略；images / structured output 不支持时 `validate` 返回 `CapabilityError`。

**流映射（wire → IR）。**

| 事件 | Chat Completions | Responses |
|---|---|---|
| `MessageStart` | 首个 chunk（`id`、`model`；`request_id` 取响应头） | `response.created`（`id`、`model`；`request_id` 取响应头） |
| 文本 | `delta.content` / `delta.refusal` → Text 块 | `output_text.delta` / `refusal.delta` → Text 块 |
| 工具调用 | `delta.tool_calls[]` 按 `index` 汇集；名字到齐后才开块，先到的参数暂存 | `output_item.added`(function_call) 开块，id 为 `call_id` + `-nyanfc-` + `fc_…`；`function_call_arguments.delta` 追加；`output_item.done` 补齐缺失的参数 |
| 推理 | 不处理 | `reasoning_summary_text.delta` → Thinking；`output_item.done` 把整条 item 作为 ProviderState 写入签名；没有摘要时以 `RedactedThinking` 保留，保证 `store: false` 下 function_call 与 reasoning 成对 |
| 用量 | `usage.prompt_tokens` 减 `cached_tokens` 为 input；`completion_tokens_details.reasoning_tokens` | `input_tokens` 减 `cached_tokens` 为 input；`output_tokens_details.reasoning_tokens` |
| 停止原因 | `stop`→EndTurn（有工具调用则 ToolUse）、`tool_calls`→ToolUse、`length`→MaxTokens、`content_filter`→Refusal、其它→`Other` | `status: completed`→EndTurn（有 function_call 则 ToolUse）、`incomplete`+`max_output_tokens`→MaxTokens、`content_filter`→Refusal、其它→`Other` |
| 结束 | 必须见到 `finish_reason`；否则 Transport / PartialStream | 必须见到 `response.completed`；否则 Transport / PartialStream |

**错误分类。** 先读结构化的 `error.code` / `error.type`，状态码兜底（401/403/404 以状态码为准，`code` 只细化 400/429/5xx）：`context_length_exceeded`（或消息含 context window / maximum context length）→ `ContextOverflow`；`rate_limit_exceeded` / `insufficient_quota` → `RateLimited`（原始 body 保留，PR 8 区分计费 429 与突发 429）；401 → `Unauthorized`；5xx / 408 → `Transient { retryable: true }`。流内的 `error` / `response.failed` 同样按 `code` 分类；已有输出时一律 `PartialStream`。

**ChatGPT（Codex）登录。** `oauth: openai-codex` 只与 Responses 搭配（Chat 报 `CodexNeedsResponses`），请求发往 `https://chatgpt.com/backend-api/codex/responses`，带 `chatgpt-account-id`、`OpenAI-Beta: responses=experimental`、`originator`、`session-id`，不带 `max_output_tokens` / `temperature`，不回放 `fc_` item id。该后端的行为取自 Pi 与官方 Codex 客户端的公开源码，未用真实账号验证（见 PR 描述“待决策”）。

**本 PR 未覆盖。** `side_query`（权限分类 / 解释、模型校验）、`count_tokens`、额度预检、非流式 fallback 仍只支持 Anthropic profile；非 Anthropic profile 下它们会返回“该 profile 不能由 Anthropic client 服务”的明确错误，而不是发出 Anthropic 形状的请求。

## 八、替换路径（不是兼容迁移）

nyan 没有用户，因此实现阶段直接删除旧入口：

- 从 `SettingsJson` 删除 `model`、`availableModels`、`modelOverrides` 等旧 provider/model 配置入口；
- 删除 `ANTHROPIC_MODEL`、`CLAUDE_CODE_SUBAGENT_MODEL` 等旧模型选择读取；
- 删除 `GlobalConfig.modelContextWindows` 和 `gateway_context_window`；
- 删除基于 Anthropic build audience 的隐式默认模型/默认 provider；
- 删除旧 `availableModels` picker allowlist 的读取；picker 只展示已解析 profile；
- 不写 migration command、dry-run、旧字段 warning、备份导入或 legacy fallback；
- 旧环境变量即使存在也不影响新 resolver，错误信息直接指出应配置 `settings.nyan.providers/models/selection`。

当前代码中的旧字段和调用链保留在本节之前仅作为审计证据，不是目标行为。

## 九、分阶段实施计划（每阶段可单独 PR）

| PR | 内容 | 独立验收 | 主要风险 |
|---|---|---|---|
| 1. schema replacement + 临时 Anthropic 兼容层 | 新增 `settings.nyan.providers/models/selection`、profile scope、`kind` 枚举、无默认 provider；删除旧 `settings.model`、`ANTHROPIC_MODEL`、`modelContextWindows` 读取和 schema；保留临时内部 Anthropic profile 兼容层（决策 22），只读现有凭据来源，让请求可发送 | 无配置时清晰引导/错误（兼容层不作为默认）；旧字段不再读取；project/local 只能引用 user/managed profile，名字不存在则报错；合入后 nyan 仍能发 Anthropic 请求 | strict schema、scope 判定、删除旧调用点不完整、兼容层残留 |
| 2. pinned model metadata + 周更 CI | 导入 Pi 快照（转换格式、Pi commit、models.dev 版本、schema version、hash）；根目录 `NOTICE` 同时列 Pi 与 models.dev；profile override 与 `nyan doctor` 展示；定时 CI 拉取上游并开 PR（决策 13–15） | 快照 hash/schema 校验；缺 contextWindow 的 profile 被拒绝；覆盖字段在 `nyan doctor` 可见；运行时无网络请求；CI 只开 PR 不合并 | 上游 schema breaking、NOTICE 漏列、周更 PR 噪声 |
| 3. credential owner and OAuth | env/`file`/`optionalEnv`/`oauth` 引用；`~/.nyan/auth/<provider>.json`（目录 0700、文件 0600、文件锁）；首批 OAuth：OpenAI Codex、OpenCode（决策 16–17）；Anthropic 只使用 API key；Windows owner-only ACL | env、OAuth、auth 文件 mock；ACL 无法验证时拒绝；无 secret 日志；provider 间账号隔离。PR 3 只做凭据存取和 mock 测试；真实模型请求从对应 provider adapter 阶段起使用 OAuth 凭据 | token schema 差异、refresh 并发、账号隔离 |
| 4. request IR + Anthropic adapter | 公共 message/tool/image/thinking/usage/error IR；Anthropic client、headers、betas、Bedrock/Vertex/Foundry 接入 adapter；**删除 PR 1 的临时 Anthropic 兼容层**（决策 22） | Anthropic mock 请求与 stream fixture；API-key credential gate；兼容层代码和引用已删除；沿用现有重试默认（决策 23） | cache prefix、beta、stream/tool/thinking 改变、兼容层删除遗漏 |
| 5. OpenAI adapters | **同一阶段**实现 Chat Completions 和 Responses；tools/images/streaming/structured output/usage/error（实现与映射表见 §7.4） | 两种协议各自 mock contract；同一 profile 不能混淆 chat/responses；错误分类与 partialStream（重试次数为 0，统一重试在 PR 8，决策 23） | 两种 wire shape、tool call streaming、retry/replay |
| 6. Gemini adapter | 独立 PR（决策 20），紧接 OpenAI 阶段且不并行；Gemini 仍按商业 kind 处理 | Gemini parts/function/thinking fixture；错误映射 `RESOURCE_EXHAUSTED` 等 | parts 与 thinking 转换、图片 inline data |
| 7. OpenAI-compatible local | vLLM/Ollama 只走 `openaiChat`；不做 Ollama native；本地 profile 可进 `mainFallbacks`（决策 21）；local profile 不能进 Decision Provider | local endpoint mock；`optionalEnv` 401 报 auth error；本地 profile 出现在 DP tier 时 schema 拒绝 | gateway 方言、本地 timeout |
| 8. retry/fallback/health | normalized errors、各 adapter rate-limit policy、统一重试控制器（默认 2 次，托管全局硬上限 5）、显式 fallback 列表；禁止隐式跨 provider。**行为变更**：Anthropic 默认重试从 10 降到 2，写入 PR 描述（决策 19、23） | 429/Retry-After/529/503/stream partial fixture；空 fallback 不切 provider；switch receipt；托管上限生效且不得超过 5 | 重复工具调用、provider header drift、默认值变更影响 |
| 9. selector and subagent | profile-only picker、main/session CLI、`inherit` snapshot、per-agent profile、按 §5.2 优先级解析、project scope enforcement | 无默认 provider；只认 profile 引用（无临时 `provider/model`）；子 agent capability validation；优先级用例全部通过 | session snapshot、优先级回归 |
| 10. commercial Decision Provider | `fast/accurate` tier 绑定商业 kind 的 profile；拒绝本地 profile；保留 off/shadow/active | local profile schema rejection；down/timeout 回确定性路径；fallback 仍只商业 | classifier 费用/延迟、来源隐私 |

没有 migration/deprecation PR。第 1 PR 完成后，旧字段和旧读取路径应不存在；后续 PR 不得重新引入兼容读取。PR 1 的临时 Anthropic 兼容层只读取 Anthropic 现有凭据来源，不读取旧字段，PR 4 删除。

## 十、风险与防护

| 风险 | 影响 | 防护 |
|---|---|---|
| Pi catalog 上游字段/许可证变化 | 能力或分发合规错误 | pin commit/schema/hash；显式更新 PR；NOTICE 首批只保留 Pi 与 models.dev 版权声明；schema breaking 必须人工审查 |
| 缺 contextWindow | 请求 overflow 或错误 compaction | profile 解析直接失败；不使用旧 200k/default/probe |
| provider rate-limit 语义不同 | 重试过多、重复请求、错误切 provider | adapter-specific policy；只在未输出时 retry；跨 provider 只走显式列表 |
| Chat/Responses 混淆 | tool/continuation 丢失 | 独立 protocol adapter 和 contract fixture；profile 明确协议 |
| 统一 OAuth broker 误用 token | token scope、刷新、存储和撤销错误 | provider-owned OAuth modules；credential profile 按 provider 隔离 |
| Windows key file ACL 不可验证 | secret 被同机其他账户读取 | ACL 验证失败即拒绝；提示使用 env/系统凭据 |
| Project settings 注入 endpoint | 项目文件劫持请求或泄露凭据 | schema/source scope 禁止 endpoint/credential file，只接受 profile name |
| 无默认 provider | 首次启动体验差 | 清晰配置引导、列出缺失字段；不静默 fallback。PR 1–4 期间临时兼容层例外 |
| 本地模型误进 Decision Provider | 分类数据外发/质量和延迟不可控 | `commercial` 由 provider `kind` 派生、用户不可写 + schema hard gate；tier fallback 同样检查 |
| OpenAI-compatible gateway 方言 | 请求字段不兼容 | protocol compatibility options 显式配置；adapter fixture；不 runtime probe |
| OAuth 文件并发写 | token 丢失/refresh race | 参考 Pi 的 file lock；provider owner 按 provider/account 串行化 |

安全边界：provider adapter 不得改变权限系统的 hard deny、路径检查、凭据 redaction 或 hook/user decision。模型能力、fallback 和 retry 也不能绕过这些边界。

## 十一、仍需用户拍板的问题

决策 1–23 已覆盖元数据形态、更新节奏、覆盖范围、首批 OAuth、凭据存储、商业判定、重试默认、Gemini 顺序和本地 fallback。本版暂无剩余待拍板问题；Anthropic 仅 API key、聚合网关归类、重试硬上限和首批 NOTICE 范围均以正文决策为准。

## 十二、验收标准

1. `settings.nyan.providers/models/selection` 是唯一持久化模型/provider 配置；无配置时不产生请求且给出明确引导/错误。
2. `settings.model`、`ANTHROPIC_MODEL`、`GlobalConfig.modelContextWindows` 和旧 alias 不再被读取。
3. profile 是唯一模型引用；project 与 local settings 只能写已有 profile 名，不能写 endpoint、credential file 或 `provider/model`。
4. 元数据快照以 checked-in 形式存在，附 Pi commit、models.dev 版本、schema version、hash；根目录 `NOTICE` 同时列出 Pi 与 models.dev 的 MIT 版权声明；运行时不联网，周更只通过 PR 生效。
5. profile 没有 contextWindow 时解析失败；有值时预请求使用保守估算，事后使用 provider usage，不使用隐式 200k。
6. Anthropic beta/cache/auth 只由 Anthropic adapter 处理，Anthropic 只接受 API key；OpenAI Chat/Responses、Gemini 不收到 Anthropic beta。
7. OpenAI Chat Completions 与 Responses 在同一阶段分别通过 contract fixture；vLLM/Ollama 只使用 OpenAI-compatible，不能选择 Ollama native protocol。
8. OAuth credential schema、refresh、storage 和 account isolation 按 provider owner 分开；Pi/CLIProxyAPI 仅为参考，不成为依赖。
9. Windows key file ACL 无法验证时拒绝使用；日志和错误不显示 secret。
10. Decision Provider 只接受 commercial profile；local profile 及其 fallback 均被拒绝。
11. 429/overload/5xx 的 retry 使用 adapter-specific 语义；stream 已产生输出后不重放；跨 provider 只在显式 fallback 列表中发生。
12. 所有 PR 阶段可单独测试；没有迁移 PR，也没有旧字段兼容读取或自动跨 provider fallback。唯一例外是 PR 1 的临时 Anthropic 兼容层，PR 4 删除。
13. 每个 PR 合入后 nyan 都能发送模型请求（PR 1 起由兼容层保证）。
14. 托管设置的全局重试上限默认 2、硬上限 5；profile 不能超过该上限。被覆盖的元数据字段在 `nyan doctor` 中可见。
15. OAuth 首批仅 OpenAI Codex、OpenCode；token 存于 `~/.nyan/auth/`，权限仅本人可读，Windows 无法验证 ACL 时拒绝；Anthropic 只支持 API key。
