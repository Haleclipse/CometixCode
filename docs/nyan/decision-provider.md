# Decision Provider 设计

状态：已定稿，未实现（2026-10-27 修订：以性能与效果为先）。后端为 EMRank（`typesafe/jev-1.14.0` 关键路径、`cloudflare/clef`
后台），已实测；决策从"粗粒度路由"扩展为**看内容的判断**（候选是否回答查询、结果是否已足够、页面是否含答案）。

设计来源：nyan-legacy 的 Jev / DecisionProvider（`references/pactile-context-governance/source/pactile/jev/`、
`source/pactile/retrieval/jev-planning.ts.txt`、`design/context-governance/implementation/03-stage-plan.zh-CN.md`、
`compression/compression-decision-chain-v1.zh-CN.md`）与 `packages/ai/src/api/{typesafe-system-one,llama-cpp-classify}.ts`。
偏离 CC 的代码用 `@nyan` 标注。

**定位**：**在工具内部，用一次约 0.4 s 的分类判断，替掉主模型要多走一整轮（约 12 s）才能做的判断，并把结论交还给模型。**
它不替主模型思考，只让工具从"机械地返回结果"变成"会判断结果好不好"——与 tool-io.md 的方向一致：工具多做机械判断，
主模型的注意力留给理解与决策。

依据（本机 26 个会话、677 个主模型轮次，2026-10-27 从 transcript 统计）：

| 观察 | 数据 | Decision Provider 的对应 |
|---|---|---|
| 主模型多一轮的代价 | 工具结果 → 下一条 assistant：p50 **12.4 s**（p25 7.9 s、p90 33 s） | 一次判断 0.4 s；20 个候选批量 0.5 s（第一节） |
| 搜索经常一次找不到 | 搜索类调用之后，**50%** 的下一个调用仍是搜索（换词重搜），27% 是读文件 | `retrieval.sufficient`：工具内判断"够不够"，不够就在**同一次调用里**补一轮 |
| 排序只说明"像" | rerank / 向量分数是相似度，不是"回答了查询" | `retrieval.answers` / `web.answers`：读候选内容判断是否回答，回答的排前并标注 |
| 页面可能答非所问 | 模型读完约 5k token 正文才发现无用 | `web.page_answers`：抓取后判断，没有答案就提示下一个候选 |
| 规则粗糙 | 正文 < 200 字符算挑战页；每 8 轮提取一次记忆 | `web.fetch_fallback`、`memory.extract_now`：按内容判断 |

收益是推算（未实测）：二次搜索每被工具内补搜吸收一次，省一个主模型轮次（约 12 s 与相应 token）。真实收益由第七节的
事后信号度量。

**边界**：不决定"要不要搜、搜什么、改不改代码"（主模型的事）；不生成文本（只做 noul / choice / score）；不碰需要精确或
可证明的决定（压缩清多少与摘要切点、私网判定、费用上限、记忆安全拦截）；不让结果变少（会删结果的点默认 shadow）；
不拖慢（与确定性路径并行，800 ms 未返回即按规则继续）。

**取舍原则**（用户 2026-10-27："不是很惧怕源码泄露，主要是要性能和表现，需要达到最佳"）：

1. **效果优先**：决策模型可以看源码正文、页面正文、工具结果——判断的质量来自看到内容。凭据仍然拦截（第六节）。
2. **只增不减的决策默认生效（active）**：追加分支、续搜、触发回退、多抓一页——错了只多花时间与钱，不丢结果。
3. **会减少结果的决策先观察（shadow）**：过滤候选、跳过提取——错了会丢东西，先积累一致率再放开。
4. **永不阻塞**：关键路径上与确定性路径并行发起，超时即按规则继续；任何时候不可用，行为与没有它时一致。

## 一、实测（2026-10-27，corp-pc 经代理，EMRank）

### 1.1 为什么是分类模型

| 方案 | 实测 | 结论 |
|---|---|---|
| 网关上的 haiku（结构化 JSON 输出，`max_tokens 40`） | 2.22 / 3.29 / 2.28 s；答案正确 | 正确但慢，不进关键路径 |
| Qwen3-Reranker 当二分类器 | 三组元决策问题（是否需要 semantic、是否值得记、是否挑战页）得分全 < 0.1，第一组排序反了 | 不能做通用判断；它只做"文档与查询的相关性" |
| 自建 Jev router（nyan-exit `:50008`） | 上游单账户 429，`all_accounts_failed` | 不可用 |
| **EMRank System One**（`POST https://emrank.tech/v1/decisions`） | 见 1.2–1.4 | **采用** |

### 1.2 质量（两轮共 30 个问题，人工给定期望答案）

| 用例 | 内容 | `jev-1.14.0` | `clef` |
|---|---|---|---|
| 第一轮：路由类（14 问） | 检索分支 / 查询意图 4 例、记忆是否值得提取 2 例、挑战页 2 例、搜索查询类型 2 例 | 13 / 14 | **14 / 14** |
| 第二轮：看内容（16 问） | 代码片段是否回答查询（读真实源码）、结果是否已足够、网页候选是否值得抓、页面正文是否含答案、记忆类型、记忆是否重复、旧工具结果是否仍需要 | 15 / 16 | 15 / 16 |

- 两轮唯一共同的错误：把"pipeline bugs are tracked in the Linear project INGEST"分成 `project`（期望 `reference`）——本身有歧义。
- jev 的 noul 概率较温和（0.02–0.95），choice 置信度高（多为 0.8–1.0）；clef 的 noul 更极端（0.008–0.977）。
- 30 个问题只能说明"方向正确、可用"，不是质量结论；质量由第七节的日志积累。

### 1.3 延迟

| 场景 | `jev-1.14.0` | `clef` |
|---|---|---|
| 首次请求（含经代理的 TLS 建连，0.8–1.2 s） | 0.96–1.16 s | 2.39 s |
| 连接复用后，1 / 2 / 4 个问题 | **0.40 / 0.49 / 0.41 s** | 0.65 / 0.69 / 0.74 s |
| 连接复用后，**一次请求 20 个候选**（每个候选一个 noul，正文约 1200 字符） | **0.49–0.60 s** | 1.47–1.56 s |
| 20 次独立请求、10 个池化连接并发 | 墙钟 1.46 s（单次 p50 0.54 s） | — |

结论：**批量放进一次请求**远好于并发多次；jev 一次判 20 个候选与判 1 个几乎同价。连接必须复用并预热（第五节）。

### 1.4 协议要点

- 请求 `{model, state, questions}`；`state` 可以是文本、JSON 对象或数组；`questions` 是字典，每个问题 `noul` / `choice` / `score`。
- 响应 `{model, answers, usage}`；`noul` 返回 0–1 概率，`choice` 返回选项、概率与置信度，`score` 返回加权等级。
- 只按成功请求的**输入 token** 计费（实测每次 198–376 token，20 候选批量约 6–8k）；RPM 按用户计。
- **默认库 User-Agent 被 Cloudflare 拦截**（403 `error code: 1010`）——请求必须带 `User-Agent: nyan/<版本>`。

## 二、legacy 的结论与本设计的取舍

legacy 把 Jev 放在 shadow，是**设计闸门而非评测结论**：放权条件是"一致率与不一致归因被 Review 接受"，而阶段 3–4 从未执行，
没有任何质量数据（`03-stage-plan.zh-CN.md:186-218`；`evidence/p1a/evaluation-receipt.json` 写明"没有连接 JEV"）。

| legacy | 本设计 |
|---|---|
| 封闭决策节点（6 个，四模块只对应 retrieval-planning） | 封闭决策点注册表（第四节），按四模块重新划分，含看内容的判断 |
| 全部 shadow，直到 Review 放权 | 每点独立 `off` / `shadow` / `active`；**只增不减的点默认 active**，会减少结果的点默认 shadow |
| 检索只能"追加分支、不能删意图、不改证据" | 保留为通用约束：**安全集合**（第三节）——决策只能在规则允许的范围内选择 |
| 置信度阈值 0.65（检索 0.75） | 保留，按决策点配置 |
| 外发审查拒绝源码、凭据、机密标记 | **只拦凭据**（第六节）；源码与正文可以发，换判断质量 |
| deadline 2.5 s / 最大 5 s，重试 1 次 | 关键路径 800 ms / 后台 2 s，不重试；与确定性路径并行 |

## 三、接口

```rust
// services/decision/mod.rs   (@nyan)
pub struct DecisionRequest {
    pub point: DecisionPoint,                  // 封闭枚举（第四节）
    pub state: serde_json::Value,              // 文本或 JSON：查询、候选正文、对话片段……
    pub questions: BTreeMap<String, Question>, // noul / choice / score；批量判断时每个候选一问
}
pub enum Question {
    Noul   { instructions: String },
    Choice { instructions: String, options: BTreeMap<String, String> },
    Score  { instructions: String, levels: Vec<String> },
}
pub enum DecisionOutcome {
    Answered { answers: BTreeMap<String, Answer>, receipt: Receipt },
    Fallback { reason: FallbackReason, receipt: Receipt },   // 调用方走确定性路径
}
#[async_trait]
pub trait DecisionBackend: Send + Sync {
    async fn decide(&self, req: &DecisionRequest, deadline: Duration, abort: AbortSignal) -> DecisionOutcome;
}
```

**每个决策点由调用方提供三样东西**，Decision Provider 只负责"问"：

1. **确定性答案**（先算出来）：回退时直接用；shadow 时作为对照。
2. **安全集合**：模型答案落在集合外 → `Fallback(out_of_safe_set)`。例如检索的安全集合不含"跳过 exact / symbol / text"。
3. **置信度阈值**：低于阈值 → `Fallback(low_confidence)`。

`FallbackReason`（封闭）：`disabled`、`shadow`、`backend_down`、`timeout`、`rate_limited`、`budget_exhausted`、`egress_denied`、
`low_confidence`、`out_of_safe_set`、`invalid_response`、`cancelled`。

- **shadow**：照常请求并记录与确定性答案是否一致，但返回 `Fallback(shadow)`——行为与关闭时相同。
- **批量**：一个决策点需要对 N 个候选各判一次时，合并为**一次请求**、每个候选一个问题（1.3：20 个候选 0.5 s）；单次最多 30 个
  候选，超出按排名截取前 30。
- **回执**（`Receipt`）只记服务方返回的值：后端、模型、延迟、usage、`X-Request-ID`、各问题的答案与置信度、与确定性答案是否一致。
  写入本地日志（第七节），不进模型上下文。
- **可复算**：生效的决策记进模块的结构化结果（检索的 `SearchOutcome`）；评测回放时注入记录的决策、不重新请求。

## 四、决策点注册表

每个点：确定性规则是默认与回退；`active` 时答案在安全集合内且置信度达标才生效。"方向"一列：**增** = 只会多做（多跑一路、多抓一页、
多搜一次），错了只多花时间与钱；**减** = 会少给 / 少做，错了会丢东西。

### 4.1 检索

| 决策点 | 问题（输入） | 确定性回退 | 安全集合 / 约束 | 方向 | 默认 | 模型 |
|---|---|---|---|---|---|---|
| `retrieval.intent` | 查询意图 choice：definition / callers / concept / literal（查询文本） | 检索 4.3 关键词规则 | 只影响 symbol 权重（1.0 / 2.0）与是否返回调用者；四路分支始终全跑 | 增 | **active** | fast |
| `retrieval.answers` | 每个候选：这段代码是否直接回答查询（noul；查询 + Inspect 后的候选片段，批量） | 无（只有 RRF / rerank 顺序） | **只标注、只排序**：回答查询的候选提到最前，并在结果里标 `(answers query)`；不删除任何候选 | 增 | **active** | fast |
| `retrieval.sufficient` | 前几名结果是否已定位到所问内容（noul；查询 + top-5 片段） | 无（不续搜） | 判"不足"时**追加**一轮：semantic 未跑则补跑；已全跑则把 `max_results` 扩到 2 倍；最多 1 轮 | 增 | **active** | fast |
| `retrieval.prune` | 每个候选：是否与查询无关（noul，复用 `retrieval.answers` 同一请求的反面） | 不删 | 删除判为无关（P(answers) < 0.05）的候选；至少保留 3 条 | 减 | shadow | fast |

- `retrieval.answers` 与 rerank 的关系：Qwen3-Reranker 给的是"相关性"分数，Decision Provider 判的是"是否回答了查询"——
  两者叠加，不替换。管线：RRF → rerank（若开）→ Inspect → **`retrieval.answers` 批量判定** → 渲染。rerank 是否默认开仍等检索评测（检索 H）。
- 原 `retrieval.branches`（"semantic 是否值得跑"）删除：四路默认全跑，问它只会少跑，不符合"效果优先"；需要补跑的情况由
  `retrieval.sufficient` 覆盖。

### 4.2 外部搜索

| 决策点 | 问题（输入） | 确定性回退 | 安全集合 / 约束 | 方向 | 默认 | 模型 |
|---|---|---|---|---|---|---|
| `web.answers` | 每个候选（title + snippet）：是否值得抓取 / 可能回答查询（noul，批量 30） | RRF / rerank 顺序 | **只排序与标注**：可能回答的提到最前、标 `(likely answers)`；不删 | 增 | **active** | fast |
| `web.page_answers` | 抓到的页面正文是否含答案（noul；查询 + 正文前 8000 字符） | 无 | 判"不含"时在结果末尾提示 `[page may not answer the query — consider fetching: <下一个候选 URL>]`；不改正文 | 增 | **active** | fast |
| `web.fetch_fallback` | 直抓正文是否为挑战页 / 空壳（noul；正文前 3000 字符） | 外部搜索 4.6：403 / 429 / 451 / 超时 / 正文 < 200 字符 | 只能把规则判成功的改判为**需要回退**（多一次 provider 调用）；不能把失败改成功 | 增 | **active** | accurate |
| `web.query_kind` | 查询类型 choice：中文 / 技术文档 / 新闻时效 / 通用 | 无 | 只用于**给 provider 加参数**（AnySearch `zone=cn`、Brave `freshness`、DeepSeek `after:`）；所有 provider 照常全量扇出。AnySearch 的 `tag` 需要配套 `library` 参数（实测缺省会 400），故不下发 | 增 | **active** | fast |
| `web.prune` | 每个候选：是否与查询无关（复用 `web.answers`） | 不删 | 删除 P < 0.05 的候选，至少保留 5 条 | 减 | shadow | fast |

- 原 `web.providers`（按查询类型**减少** provider 省钱）删除：效果优先，默认全量扇出（Decision #44）；查询类型只用来给每家加更好的参数。
- 外部搜索的费用上限（外部搜索 3.5）不变，仍是确定性的。

### 4.3 记忆

| 决策点 | 问题（输入） | 确定性回退 | 安全集合 / 约束 | 方向 | 默认 | 模型 |
|---|---|---|---|---|---|---|
| `memory.extract_now` | 刚结束的这一轮是否含值得长期记住的内容（noul；本轮用户 / 助手文本） | 记忆 M2：每 8 轮或 token 增长 ≥ 30k | 判"有"时**提前**触发提取（不等 8 轮）；每 3 轮最多提前 1 次 | 增 | shadow | accurate |
| `memory.extract_skip` | 节流到点时，这一段是否没有可记内容（noul） | 按规则提取 | 只能跳过节流触发的提取；连续跳过 ≥ 3 次后强制一次；压缩前的强制提取（记忆 M2.1）不可跳过 | 减 | shadow | accurate |
| `memory.type` | 新记忆的类型 choice：user / feedback / project / reference / none | 提取 agent 自判 | 与提取 agent 不一致时转为候选（M4），由用户确认；`type: user` 仍由硬规则判为候选 | 增 | shadow | accurate |
| `memory.duplicate` | 新记忆是否与已有条目重复（noul；新条目 + 同类型已有条目） | 提取 agent 自判 | 判重复时转为候选（提示"可能与 X 重复"），不自动合并、不删除 | 增 | shadow | accurate |
| `memory.consolidate` | 现在是否该合并（noul；新文件数、上次合并时间、索引行数） | 记忆 M9：24 h 且 ≥ 5 个新文件 | 只能**提前**到 ≥ 12 h 且 ≥ 3 个新文件，或推迟到 ≤ 72 h | 增 / 减 | shadow | accurate |

- "转为候选"在这里是**增**：多给用户一次确认机会，不丢记忆（候选文件照常写入，只是不进索引，记忆 M4）。
- 五个记忆点统一 **shadow-only**：`active` 也只记录、不应用；开关只由 `decision.points.<point>.mode` 控制（`memory.decisionShadow` 已删除）。

### 4.4 压缩

| 决策点 | 问题（输入） | 确定性回退 | 安全集合 / 约束 | 方向 | 默认 | 模型 |
|---|---|---|---|---|---|---|
| `compaction.keep` | 每个掩码候选：这条旧工具结果对当前任务是否仍然需要（noul；当前任务摘要 + 候选前 1500 字符，批量） | 压缩 C2：从旧到新清到阈值以下 | 只调整**清理顺序**：判"仍需要"的候选排到最后清；仍须清到 `measure ≤ 阈值 − 余量`，清多少不变 | 增 | shadow | fast |

- 只改顺序、不改"清多少"，所以 C3 的 cache 约束（一次清到位、之后字节稳定）不受影响；但它改变"清哪些"，前缀变化的位置会不同
  （最早被清位置可能更靠后，cache 失效量**更小**），所以先 shadow 观察。决策在掩码发生的那一轮做一次，结果随 `content-replacement`
  持久化，之后字节稳定、不重复询问。
- **超时与并发**：`compaction.keep` 属于关键路径决策点（`DecisionSettings::is_critical_point`），其超时预算由 `settings.nyan.decision.deadlineMs.critical` 控制（默认 800 ms）。Shadow 评估通过共享后台 worker 异步执行（fire-and-forget，不阻塞主 query 循环），限制并发数（超限跳过并计数），退化配置（`threshold <= 0`）记警告且不记录，每条 shadow 记录包含 `timeoutMs`、`mode="shadow"` 以及反映真实结果的 `outcome`（`"answered"` / `"fallback"`）。
- 其余压缩决定（是否摘要、摘要切点、保活）仍是确定性的，见下表。

### 4.5 不交给 Decision Provider 的决定

| 决策 | 理由 |
|---|---|
| 压缩：清多少、何时掩码、是否摘要、保活是否值得 | 有精确的 token 账；是 cache 约束的核心，必须可复算 |
| 压缩：摘要切点 | 必须保证 tool_use / tool_result 成对，规则可证明 |
| 外部搜索：私网判定、费用上限 | 安全与费用边界只能是确定性的 |
| 记忆：读时 / 写时安全检查（M3） | 安全拦截必须确定性 |
| 查询改写、扩展、拆分 | 需要生成文本，分类模型做不了；主模型自己会改写重搜 |

### 4.6 宿主运行时的全部候选场景

四个模块是第一阶段，不是 Decision Provider 的边界。宿主里凡是"输入内容 → 封闭答案"的判断都可以接入；下面是对当前代码的
第一轮普查。**候选不等于立即启用**：先接入统一接口，按 `off / shadow / active` 逐点放权。

| 场景 | 当前代码 / 当前决策 | DP 适合做什么 | 安全边界与默认 |
|---|---|---|---|
| **Auto Mode / YOLO 权限分类** | `utils/permissions/yolo_classifier.rs:853`；当前是完整 side query，支持单阶段或 XML 两阶段（stage 1 快速判定、stage 2 thinking），输出 `should_block + reason`；解析失败默认阻断 | 作为 stage 1 的快速风险分类：`safe_readonly / safe_local_write / risky`，或判断是否需要进入昂贵的 stage 2；读取命令、上下文和工具输入 | `dangerous_patterns.rs`、`read_only_command_validation.rs`、权限规则仍是硬门；DP **不能单独放行**不确定命令；默认 shadow，确认无 false-safe 后再 active |
| **Bash classifier** | `utils/permissions/bash_classifier.rs:67` 当前是 disabled stub | 未来替代 disabled classifier 的结构化判断，或给权限解释器提供风险标签 | 不能绕过硬 deny；先只做 advisory / ask 路径，不直接 allow |
| **Permission explainer** | `utils/permissions/permission_explainer.rs:222-300` 用 side query 生成 explanation / reasoning / risk / riskLevel，超时 10 s 返回 None | 不接 Decision Provider；改为 utility 角色 `permission_explainer`（默认 @light） | 解释文本由 utility 模型生成，硬安全边界不变 |
| **权限规则与安全解析** | `permissions.rs` 规则匹配、`dangerous_patterns.rs`、`read_only_command_validation.rs`、路径 / UNC / exfil 检查 | 可把结果汇总为风险上下文，供上面的 DP 分类使用 | **不交给 DP 覆盖**：硬安全边界、路径越界、凭据外发、命令 flag 解析必须确定性 |
| **Prompt suggestion 是否值得生成** | `prompt_suggestion/prompt_suggestion.rs:191` 生成用户下一句；文本由小模型生成 | 在生成前判断"下一步是否足够明确、是否值得花一次生成调用" | 只影响是否显示建议，不影响会话；可 active，默认先 shadow |
| **Speculation 是否启动** | `prompt_suggestion/speculation.rs:264` 启动 fork，已有只读 / 写入 overlay / 权限边界 | 判断用户下一步是否明显、是否值得预计算；可判断候选路径是否可能很快撞权限边界 | overlay、只读 Bash、写权限边界继续由确定性代码执行；DP 不能让 speculation 绕过边界 |
| **Context suggestions 选择 / 排序** | `utils/context_suggestions.rs:29` 是 token 阈值、严重度和节省量的纯规则 | 多条建议同时出现时，按当前任务选最有用的一条或排序 | 低收益；规则先保留，DP 只做 UI 排序，不改变压缩和上下文（不接入：DP1 已删除该点） |
| **Session title / rename** | `session_title.rs:93`、`commands/rename/generate_session_name.rs:16` 生成标题 / kebab-case 名称 | 不适合替代；可判断是否值得重新命名或标题是否需要更新 | 最终输出是文本，继续用生成模型 |
| **Tool-use summary** | `services/tool_use_summary.rs:32` 生成移动端短标签 | 不适合替代；可以在输入很短时跳过生成 | 生成文本，不是分类；失败不影响主轮 |
| **Agent routing** | Agent 工具的 `subagent_type` 通常由主模型给出；built-in agent 有 Explore / Plan / Verify 等 | 没有明确 `subagent_type` 或候选冲突时，从封闭集合选择 agent 类型 | 不能覆盖用户显式选择；默认 shadow；错误只影响路由，不改权限（不接入：DP1 已删除该点） |
| **Plan / verification 判断** | Plan 与 Verify agent 由提示词和主模型驱动；`VerificationAgent` 有独立 query source | 判断计划是否已满足、是否需要再验证、验证结果是否值得回主线程 | 只能追加验证，不能自行宣称已完成；默认 shadow（不接入：DP1 已删除该点） |
| **Skill 选择** | Skill 命令由显式 `/name` 或主模型调用；动态 skill trigger 是路径 / 编辑事件规则 | 对多个候选 skill 做相关性排序，或判断是否值得加载；不自动触发有副作用的 skill | 显式 skill 不经过 DP；自动触发默认 off / shadow，不能替代用户授权（不接入：DP1 已删除该点） |
| **Model validation** | `utils/model/validate_model.rs:16`：allowlist + 真实 max_tokens=1 probe + 缓存 | 不需要；可把错误响应分类为 retryable / not-found 以改善提示，但现有 `ApiError` 已足够 | 模型是否可用是外部事实，保持确定性 probe |
| **Model fallback / retry** | `services/api/with_retry.rs` 按错误类型、query source 和配置决定 | 只可做非安全的 provider / route 选择建议 | retryable、最大重试、预算和凭据错误必须由错误类型与配置决定 |
| **Context / compaction admission** | `context_suggestions.rs` 阈值；autocompact / circuit breaker 用 token、错误和次数 | 可解释"是否值得提前提醒 / 是否建议用户 compact" | 不改变压缩准入、掩码数量、熔断规则；这些已有精确数值 |
| **Hooks / permission prompts** | 用户 hook 的 exit code / JSON schema、权限响应、Plan 选择是外部契约 | 可在 hook 结果后做非权威的摘要或分类 | **不能覆盖 hook、用户选择或权限响应** |
| **Todo / task reminders** | `attachments.rs` 按计数和状态生成 reminder | 可判断 reminder 是否值得展示、摘要哪些任务最相关 | 低收益；不改变任务状态与完成判定（不接入：DP1 已删除该点） |

#### 4.6.1 权限场景的专门规则

权限是唯一需要单独收紧的类别。Decision Provider 可以看完整命令、cwd、工具输入和上下文，但采用三层结构：

1. **硬规则先行**：危险命令、路径越界、凭据 / 网络外发、非只读命令解析仍由确定性代码直接 deny / ask；DP 无权翻转。
2. **DP 快速分流**：对硬规则没有结论的命令分类；高风险或低置信度进入现有 YOLO / 用户确认，明显安全的只读命令才有资格
   缩短后续流程。DP 的"safe"只能减少解释 / 第二阶段成本，不能凭一条模型答案放行此前未被硬规则允许的命令。
3. **完整分类器 / 用户确认**：保留现有 YOLO 的 reason、thinking 和 fail-closed 行为；DP 失败、429、超时一律走这条路径。

第一次权限探测（2026-10-27，小样本，不是质量结论）：`jev-1.14.0` 对 16 个手工命令的风险 choice 判定 15/16，破坏性 noul
13/16，测试样本中没有把明显危险命令判成安全；`clef` 的风险 choice 15/16，并把写 `~/.bashrc` 判成安全。说明 DP 可以做
**快速分流**，但绝不能在没有硬规则与完整分类器的情况下直接承担 allow。

#### 4.6.2 统一接入顺序

所有新增场景使用同一个适配器，不在业务代码里直接调用 EMRank：

```
硬规则 / 现有确定性结果
  → 组装 DecisionRequest（可批量）
  → Decision Provider（off / shadow / active）
  → 安全集合 + 置信度校验
  → 只应用允许的增量动作
  → 失败 / 超时 / 低置信度 → 原确定性路径
```

生成文本的模块（标题、建议、解释、摘要）可以在生成前使用 DP 做"是否值得生成"，但生成本身仍由文本模型完成；安全模块
可以使用 DP 做分流、排序和升级判断，但不能让 DP 放宽硬安全边界。

## 五、后端

只有一种协议：System One。

| 后端 | 形态 | 状态 |
|---|---|---|
| `emrank`（主） | `POST https://emrank.tech/v1/decisions`，Bearer key 按文件引用 | 可用（第一节） |
| `local`（不在首批） | llama.cpp `/completion`（`n_predict: 1`、`n_probs`）读标签 logprob，对外仍是 System One 形态 | EMRank 长期不可用时再部署 |

### 5.1 模型

| 别名 | 模型 | 用于 |
|---|---|---|
| `fast` | `typesafe/jev-1.14.0` | 关键路径（检索、外部搜索、压缩）：连接复用后 0.4 s，20 个候选批量 0.5–0.6 s |
| `accurate` | `cloudflare/clef` | 后台与"增"类低频点（记忆、`web.fetch_fallback`）：noul 区分度更高，0.65–0.75 s |

- 关键路径固定 `jev-1.14.0`（`latest` 会随上游升级，升级后日志前后不可比）；升级是一次显式配置变更，日志按模型 ID 分段。
  `clef` 目前没有固定版本 ID，日志记录响应里的 `model`。
- 同一决策点不混用模型，不做"失败换另一个重试"。

### 5.2 连接

- 进程内共享的 keep-alive 连接池（reqwest `Client`，`pool_idle_timeout` 90 s，`pool_max_idle_per_host` 4）。
- **预热**：会话启动后第一次用户输入时，后台发一个最小请求；空闲超过 90 s 后的第一次关键路径决策若赶不上 deadline 就回退，
  同时后台重新预热。
- 请求带 `User-Agent: nyan/<版本>`（默认 UA 被拦截）。
- 出站经本地代理（与外部搜索相同的代理构建方式，外部搜索 4.2 规则 1）。
- 可用性由服务健康状态机管理（README 第一节第 9 条）：429 → `rate_limited` 退避；402 → `budget_exhausted`；401 → `backend_down`。

### 5.3 费用与限速

- 单次决策约 200–400 输入 token；批量 20 个候选约 6–8k。按每次 `CodeSearch` / `WebSearch` 2–3 个决策估，单会话约 50–150 万
  输入 token。上限 `maxInputTokensPerSession`（默认 300 万）与 `maxCallsPerSession`（默认 600）只防失控，正常使用碰不到。
- shadow 也照常请求（要积累数据）；`mode: "off"` 才不发。
- 响应头 `X-Request-ID` 记入回执。

## 六、外发

`state` 发给 EMRank（第三方，再转给 TypeSafe / Cloudflare）。按用户的取舍，**源码正文、页面正文、对话文本、工具结果都可以发**。
只拦一类：**凭据**。

- 复用记忆 M3 的 secret scanner（`sk-…`、`ghp_…`、`AKIA…`、私钥块、JWT、`key=value` 中 key 含 token / secret / password / api key 等）。
- 命中时**只遮蔽命中的片段**（替换为 `<redacted>`）后照常发送，不整体拒绝——判断仍然能做，凭据不出本机。
- 凭据类路径（`.env`、`*.pem`、`id_*`、`credentials*`）的文件内容不进 `state`（检索 4.5 本就不索引它们）。

## 七、日志与放权

- 每次决策写一行 `$NYAN_CONFIG_DIR/decision/decisions.jsonl`（所有点共用同一文件，记录自带 `point`）：时间、模型、延迟、答案与置信度、确定性答案、是否一致、事后信号；
  `state` 只记 hash 与长度。保留 30 天。
- **事后信号**（事后探针进程内保存 30 分钟，`/clear` 清空）：
  - 决策保留候选时立即追加一行 `{"event":"probe","probeId","point","kind","candidateCount"}`；
  - 之后 `Read` 命中文件候选、`WebFetch` 命中 URL 候选（含重定向后的 final URL）时追加一行
    `{"event":"posthoc","probeId","point","kind","identity"}`，同一候选只记一次。文件路径按 canonicalize
    比较，URL 忽略大小写、fragment 与末尾斜杠；
  - 已接入：`retrieval.answers`（被标"回答查询"且实际展示的候选）、`retrieval.sufficient`（追加一轮后新出现且展示的结果）、
    `web.answers`（最终可见且被标记的候选）、`web.page_answers`（提示里建议的下一个候选）；
  - 尚未接入：`web.fetch_fallback`（正文长度对比，不是"之后是否使用"）、`memory.*`（候选身份在 `/memory` 交互里）、
    `compaction.keep`（工具结果与文件路径的映射未定义）。
- **`/decision [point]` 命令**：读统一 jsonl，输出每个点的样本数、answered / fallback、一致与不一致计数、p50 / p95 延迟，
  以及 `probes` / `used`（保留候选的决策数与其中至少一个候选后来被使用的决策数）；可传点名校验单点。不做自动放权。

## 八、配置

`settings.nyan.decision`：

```jsonc
{
  "enabled": true,
  "emrank": {
    "url": "https://emrank.tech/v1/decisions",
    "apiKeyFile": "~/.nyan/keys/emrank",
    "models": { "fast": "typesafe/jev-1.14.0", "accurate": "cloudflare/clef" },
    "poolIdleTimeoutMs": 90000,
    "prewarm": true
  },
  "deadlineMs": { "critical": 800, "background": 2000 },
  "maxBatch": 30,
  "maxCallsPerSession": 600,
  "maxInputTokensPerSession": 3000000,
  "points": {
    "retrieval.intent":     { "mode": "active", "model": "fast",     "minConfidence": 0.7 },
    "retrieval.answers":    { "mode": "active", "model": "fast",     "minConfidence": 0.6 },
    "retrieval.sufficient": { "mode": "active", "model": "fast",     "minConfidence": 0.7 },
    "retrieval.prune":      { "mode": "shadow", "model": "fast",     "maxProb": 0.05, "keepAtLeast": 3 },
    "web.answers":          { "mode": "active", "model": "fast",     "minConfidence": 0.6 },
    "web.page_answers":     { "mode": "active", "model": "fast",     "minConfidence": 0.7 },
    "web.fetch_fallback":   { "mode": "active", "model": "accurate", "minConfidence": 0.8 },
    "web.query_kind":       { "mode": "active", "model": "fast",     "minConfidence": 0.7 },
    "web.prune":            { "mode": "shadow", "model": "fast",     "maxProb": 0.05, "keepAtLeast": 5 },
    "memory.extract_now":   { "mode": "shadow", "model": "accurate", "minConfidence": 0.8 },
    "memory.extract_skip":  { "mode": "shadow", "model": "accurate", "minConfidence": 0.8 },
    "memory.type":          { "mode": "shadow", "model": "accurate", "minConfidence": 0.7 },
    "memory.duplicate":     { "mode": "shadow", "model": "accurate", "minConfidence": 0.7 },
    "memory.consolidate":   { "mode": "shadow", "model": "accurate", "minConfidence": 0.7 },
    "compaction.keep":      { "mode": "shadow", "model": "fast",     "minConfidence": 0.7 },
    "permissions.yolo_stage1":       { "mode": "shadow", "model": "fast",     "minConfidence": 0.9 },
    "prompt_suggestion.generate_gate": { "mode": "shadow", "model": "fast",  "minConfidence": 0.75 }
  },
  "logRetentionDays": 30
}
```

- `enabled: false` 或某点 `off`：不发请求，行为与没有 Decision Provider 时相同。
- 每台机器各自配置；key 在 `~/.nyan/keys/emrank`（0600，corp-pc 已就位，nyan-pc 复制同一文件）；读取时 trim。
- **关键路径**（检索、外部搜索、压缩、权限）的 800 ms 是硬上限：与确定性路径**并行发起**，到点未返回就按规则继续，不等待、不重试。
  `retrieval.answers` / `web.answers` 在 Inspect / 融合完成后发起，此时其余工作已结束，等待它的 0.5 s 计入工具耗时；
  `retrieval.sufficient` 判"不足"时追加的一轮在同一个工具调用内完成（总 deadline 不变）。

## 九、明确不做

| 项 | 理由 |
|---|---|
| 自动放权（按一致率自动从 shadow 升 active） | 行为变更由用户看 `/decision` 后决定 |
| 多后端聚合 / 投票 | 加延迟；决策要快 |
| 生成型决策（查询改写、摘要、参数值） | 分类模型不生成文本；只在封闭选项内选择 |
| 用决策减少 provider / 跳过检索分支 | 效果优先；只用于加参数、补跑、排序 |
| legacy 的 21 种回退码、tile / scheduling / review 等节点 | 本项目只有第四节的决策点 |

## 十、实现切片

| 切片 | 内容 | 主要写集 | 依赖 |
|---|---|---|---|
| A | 接口、批量问题、安全集合与阈值、shadow 语义、凭据遮蔽（复用 secret scanner）、会话上限 | 新 `services/decision/{mod,egress}.rs` | 公共 H、记忆 A（secret scanner） |
| B | `emrank` 后端：连接池与预热、UA、模型别名、401 / 402 / 429 映射、`X-Request-ID` | `services/decision/backends/emrank.rs` | A |
| E | 决策日志、事后信号、`/decision` 命令 | `services/decision/log.rs`、`commands/decision/` | A |
| F | 接入检索：`intent`、`answers`（Inspect 后批量）、`sufficient`（追加一轮）、`prune` | 检索切片 B / F 的管线 | A、检索 B |
| G | 接入外部搜索：`answers`、`page_answers`、`fetch_fallback`、`query_kind`（provider 参数）、`prune` | 外部搜索切片 E / H 与 WebFetch 返回 | A、外部搜索 E |
| H | 接入记忆：`extract_now`、`extract_skip`、`type`、`duplicate`、`consolidate` | 记忆切片 C / D | A、记忆 C |
| I | 接入压缩：`compaction.keep`（掩码候选排序） | 压缩切片 A 的候选选择 | A、压缩 A |
| J | 接入权限：仅 `permissions.yolo_stage1`（先 shadow，active 只能升级，永不单独放行）；`permissions.explain_risk` 不接，风险解释走 utility 角色 `permission_explainer`（默认 @light）；硬规则和完整分类器 fail-closed 不变 | `utils/permissions/`、`services/decision` 适配层 | A、B、权限现状已读 |
| K | 接入建议：`prompt_suggestion.generate_gate`（默认关；开启后先 shadow，active 只能跳过生成）。`speculation.start` 不接，`context_suggestions.rank` 删除 | `services/prompt_suggestion/` | A、B |

A 进批次 1；B、E 并行；F、G、H、I 跟随四模块调用点；J、K 作为宿主运行时批次后续接入。`chat` / `local` 后端不实现。

## 十一、验收

1. `enabled = false`、所有点 `off`、或后端 Down 时，四个模块的行为与没有 Decision Provider 时逐字节一致。
2. shadow 点照常请求并记日志，行为与 `off` 相同；active 点答案在安全集合内且置信度达标才生效，否则回退。
3. 关键路径决策超过 800 ms 即回退；检索 / 搜索的总耗时增加不超过 800 ms（实测预期约 0.5 s）。
4. 批量判定：20 个候选合并为一次请求，预热后 p50 < 800 ms（实测 jev 0.49–0.60 s）。
5. `retrieval.answers` / `web.answers` 只重排与标注，不减少返回条数；`prune` 点（shadow 关闭前）不删任何候选。
6. `retrieval.sufficient` 判"不足"时最多追加 1 轮；三个确定性分支始终运行。
7. `web.query_kind` 只给 provider 加参数，所有 enabled provider 照常参与。
8. `memory.extract_now` 每 3 轮最多提前 1 次；`memory.type` / `duplicate` 只把记忆转为候选，不删除、不合并。
9. `compaction.keep` 只改变清理顺序、不改变清理量；决定随 `content-replacement` 持久化，之后不重复询问、字节稳定。
10. 含凭据的片段被遮蔽为 `<redacted>` 后发送；凭据类路径的文件内容不进 `state`。
11. 请求带 `User-Agent: nyan/<版本>`；日志按响应 `model` 分段统计。
12. 离线测试：后端 URL 可注入，`TcpListener::bind("127.0.0.1:0")` 起 mock，覆盖 answered / 批量 / 各回退原因。
13. 权限场景：DP 的 safe / low-confidence / backend_down 都不能绕过硬 deny；DP 失败时进入现有 YOLO 或用户确认，不得变成 allow。
14. `prompt_suggestion.generate_gate` / `speculation.start` 只影响是否预计算，不改变用户输入、文件 overlay、权限边界或主会话消息。
15. 生成型调用（session title、tool-use summary、permission explanation、prompt suggestion 文本）仍由文本模型生成，Decision Provider 只可控制是否调用或提供结构化标签。
16. 所有宿主运行时决策点都能独立设为 `off`，关闭后保持当前代码路径；`shadow` 不改变用户可见行为。

## 十二、待验证（实现切片的第一步，不阻塞设计）

- 各决策点的真实一致率与事后信号（第一节只有 30 个问题）。
- nyan-pc 到 EMRank 的建连与复用延迟（只在 corp-pc 测过）。
- EMRank 的模型倍率与 RPM 档位（需登录控制台）；据此复核会话上限。
- `retrieval.answers` 与 Qwen3-Reranker 叠加后的排序收益——并入检索评测（检索 H）。
