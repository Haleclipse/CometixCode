# nyan 二次开发：压缩、记忆、检索、外部搜索、Decision Provider、工具 I/O

本目录是 nyan 面向压缩、记忆、检索、外部搜索、Decision Provider 与工具 I/O 的二次开发设计与实施计划。设计思想来自
nyan-legacy（pactile context-governance、旧 roadmap N3–N8），实现以 CC 现有代码为基座。

**原则：对齐 legacy 的思想，不照搬它的形态。** legacy 的具体形态（RetrievalPack 词汇、13 项检查表、EvidenceRecord /
citationPointer、状态字段、回收站、远端协调器、按美元算的经济账……）是为 Pactile 的 Kernel、多客户端与评测平面设计的。
在本项目里，同一个思想优先用 CC 已有的结构表达：工具结果用 CC 的简洁文本风格；"候选记忆"就是不在 MEMORY.md 索引里的
文件；网页证据就是按 hash 存的快照文件；cache 保活复用 CC 的分叉共享机制、从 API usage 读 TTL。每份文档的
"明确不做"一节记录了哪些形态没有搬过来、为什么。

| 文档 | 内容 | 状态 |
|---|---|---|
| [compaction.md](compaction.md) | 压缩：先清后摘、掩码、cache 不因时间失效、cache 保活 | 定稿 |
| [memory.md](memory.md) | 记忆：后台提取、合并、写入安全、候选（= 不在索引里的记忆文件） | 定稿 |
| [retrieval.md](retrieval.md) | 检索：`CodeSearch` 融合工具、本机索引、Inspect、semantic（两机可用、可降级） | 定稿 |
| [external-search.md](external-search.md) | 外部搜索：七家 provider 聚合召回 + 精排、SSRF 安全 WebFetch + provider 回退、原文快照与逐字摘录 | 定稿 |
| [tool-io.md](tool-io.md) | 工具 I/O：Read / Grep / Glob / Bash 的完整性尾行、确定性顺序、编码检测、批量读、Bash 头尾保留（借鉴 fastctx v0.2.6）；检索与搜索做内置工具、不做 MCP / Skill 的结论 | 定稿 |
| [decision-provider.md](decision-provider.md) | 快速决策模型：四模块的决策点注册表、安全集合与确定性回退、看内容的判断（候选是否回答查询、结果是否足够、页面是否含答案、记忆类型与重复）、EMRank 后端（jev 关键路径、clef 后台，已实测） | 定稿 |
| [cicd.md](cicd.md) | CI/CD：hosted-only、一次构建分片执行、fail-closed Merge gate、零 secret、tag 门禁 + 已验证字节发布；含 master 基线 26 个失败的实测清单 | 设计稿 |
| [cost.md](cost.md) | 费用统计：请求层统一记账、目录价 + profile 覆盖、1h 写入 = 输入 × 2、usage 记录与 resume 重放、`--max-budget-usd` | 定稿 |
| [agents-md.md](agents-md.md) | 指令文件：彻底移除 CLAUDE.md 及其变体，统一采用通用 AGENTS.md 规范 | 定稿 |

## 一、通用约定

1. **偏离标记**：所有有意偏离现有行为的代码位置加 `// @nyan: <原因>` 注释，统一使用 `@nyan` 标记。
2. **配置**：全部挂在 `SettingsJson` 新增的一个正式字段 `nyan` 下（settings 是严格 schema，未知顶层键会报错，
   `utils/settings/validation.rs:454`）：`nyan.compaction` / `nyan.cache` / `nyan.memory` / `nyan.retrieval` / `nyan.web` / `nyan.decision` / `nyan.toolIo`。
   凭据只按文件路径或环境变量名引用，不写进 settings 正文。
3. **回退开关**：每个模块都能关掉回到 CC 行为（`nyan.compaction.mask=false`、`summaryStyle="cc"`、
   `nyan.memory.extract=false`、`nyan.retrieval.enabled=false`……），用于对照与回退。
4. **cache 是第一约束**：不允许任何因时间流逝而丢 cache 的机制；会改变请求前缀的操作必须换来足够的 token 节省，
   且之后字节稳定。子代理 / 分叉（摘要、提取、合并）优先共享主会话 cache。
5. **失败不伪装**：所有降级、跳过、截断都以具名状态出现在结果里，不变成空结果或静默成功。
6. **存储位置**：全部在 `$NYAN_CONFIG_DIR`（本机为 `~/.nyan`）下，永不写进工作树。
   - 记忆：按 canonical git root（沿用 CC memdir）；
   - 检索索引：按 worktree 根（不同 worktree 内容不同）；
   - 网页快照：全局一份、内容寻址。
7. **两台机器**：nyan 在 nyan-pc 与 corp-pc 上都用。`~/.nyan`（settings、keys、记忆、索引、快照）各机一份、
   **不同步**（接受：两机的工作区与会话本就不同）。任何依赖某台机器的能力都要可降级（第 9 条）。

### 公共组件（各模块共用，只定义一次）

8. **cache 共享分叉**（压缩 C6 / C12、记忆 M2 / M5 引用）：前缀与主线程逐字节相同、目的是命中主线程 cache 的分叉。
   判别按 `QuerySource`（`constants/query_source.rs`），名单是封闭的：

   | `QuerySource` | 来源 | 现状 |
   |---|---|---|
   | `Compact` | 摘要（压缩 C6） | 已有，`compact.rs:1044` |
   | `SideQuestion` | `/btw` | 已有，`side_question.rs:94` |
   | `PromptSuggestion`、`Speculation` | CC 的建议 / 预测 | 枚举已有 |
   | `ExtractMemories`、`AutoDream` | 记忆提取 / 合并（记忆 M2 / M5） | 枚举已有，nyan 实现 |
   | `CacheWarm`（新增） | 保活（压缩 C12） | nyan 新增 |

   对名单内的分叉，统一三条规则：
   - **不带 `X-Claude-Code-Agent-Id`**（压缩 C13-3、切片 G），让网关用主线程的 `prompt_cache_key`；
   - **只重放、不新增掩码**：继承主线程的 `ContentReplacementState`（`forked_agent.rs:204`），跳过掩码候选选择与单消息
     预算替换的新决定（压缩 C2）；
   - 判别函数 `is_cache_sharing_fork(source)` 放在 `utils/forked_agent.rs`，三处共用，不各自写名单。

9. **服务健康状态机**（检索 3.2、外部搜索 3.2.1 / 3.3 引用）：所有自建服务（embedding、rerank、Qdrant、SearXNG）与
   Decision Provider（第 10 条）共用一份实现 `services/service_health.rs`：`Ready ⇄ Down(until)`、连接超时与请求超时
   分开、退避 30 s 起翻倍至 5 min、到期后由下一次使用触发一次后台探测、启动不探测。每个服务一个实例，进程内共享。
   是批次 1 的独立切片（"公共 H"），检索 F / G0、外部搜索 E / F / H、Decision Provider 都依赖它。
   **范围**：本条只管自建服务与 Decision Provider。六家商业 provider（AnySearch、Brave、DeepSeek、Exa、Firecrawl、
   Tavily）的健康状态另有一套按 `(provider, 能力)` 分键、软/硬失败分级、真实调用试探的机制，见
   [external-search-provider-health.md](external-search-provider-health.md)。

10. **Decision Provider**（[decision-provider.md](decision-provider.md)）：四个模块以及宿主运行时里需要"快速判断"的场景都可接入
    同一个分类后端（权限分流、建议 / speculation 门控、agent / plan / skill 路由、结果质量判断等）；统一接口、批量、超时、回退与评测接缝。
    硬安全规则和生成型文本不由它替代；不可用时一律回退到当前确定性 / 原模型路径。

## 二、开发与验证流程（每个切片）

1. 读相关代码与调用链，确认文档里引用的 `file:line` 仍然成立（上游可能已变）。
2. 实现；偏离处加 `@nyan`。
3. 受影响的已有测试改为断言新行为；新行为各有测试。
4. 跑测试——**必须用 justfile 的隔离方式**（`NYAN_CONFIG_DIR` 指向 `target/test-home`，清掉宿主的模型 / 网关
   环境变量），否则测试会读写真实的 `~/.nyan`：
   - 有 `just` 与 `cargo-nextest` 时：`just t <模块名>`，收尾跑 `just test`；
   - 没有时：`cargo test --lib --no-run` 后，按 justfile `t` recipe 的环境变量逐个进程跑受影响模块的测试。
   - 已知基线失败（与本工作无关）：`components::prompt_input` 下 4 个测试（`arrows_matches_official_cursor_before_history`、
     `color_only_banner_matches_official_empty_text_border`、`renders_live_swarm_banner_projection`、
     `vim_delete_operator_removes_logical_line`）。
5. 改动文件过 rustfmt（只看自己的 hunk，上游本身有未格式化代码）。
6. 每个切片一个 commit，message 说明"为什么"。

## 三、实施顺序

依赖关系与可并行性（同一行内的切片写集不重叠，可并行）：

| 批次 | 切片 | 说明 |
|---|---|---|
| 0 | 公共：`SettingsJson.nyan` 字段与各模块配置结构体；`@nyan` 约定；`is_cache_sharing_fork`（第一节第 8 条） | 所有模块的前置 |
| 1 | 压缩 A（掩码 + 移除 time-based + `METADATA_MARKERS`）、压缩 C（Read 去重）、压缩 D（摘要指令与校验）、压缩 G（子代理 `X-Claude-Code-Agent-Id`）、记忆 A（写入安全）、记忆 B（候选机制）、外部搜索 A（安全抓取器）、外部搜索 D（七家 provider 适配器）、检索 A（库接口、结果模型、Inspect、渲染）、检索 C（本机索引）、检索 G0（rerank 共享客户端）、公共 H（服务健康状态机，第一节第 9 条）、DP A（Decision Provider 接口与回退） | 写集互不重叠 |
| 2 | 压缩 B（估算 + 接线）、压缩 F（cache 保活）、外部搜索 B（快照 + 原文返回）、外部搜索 E（聚合 WebSearch）、外部搜索 G（配置与 `is_enabled`）、检索 B（exact + `CodeSearch` 工具）、检索 D（symbol）、检索 E（text） | 依赖批次 1 |
| 3 | 压缩 E（保留尾部 + 回执）、记忆 C（提取 agent）、记忆 D（合并 agent）、外部搜索 C（逐字摘录）、外部搜索 F（网页精排）、外部搜索 H（fetch provider 回退）、检索 F（semantic + 两机访问 + 回退） | |
| 4 | 记忆 E（`/memory` UI）、检索 G（代码检索接入 rerank，默认关）、检索 I（接入压缩名单 / 记忆 M6 / 后台子代理白名单） | |
| 1–2 | 工具 I/O A / E（编码检测、Bash 头尾保留）进批次 1；B / D（Read、Grep / Glob 的尾行与顺序）进批次 2；C（批量 Read）在压缩 C 之后；F 收尾（tool-io.md 第七节） | |
| 2–4 | DP B / D / E（emrank、chat 后端，决策日志与 `/decision`；DP C `local` 不在首批）；DP F / G / H 跟随检索 B、外部搜索 E、记忆 C 的调用点 | DP A 之后 |
| 后续 | 检索 H（评测） | 后续单独安排 |

**跨模块写集重叠**（不在同一批次，按批次先后顺序做即可，后做的切片在先做的基础上追加）：
- `utils/session_storage.rs` 的 `METADATA_MARKERS`：压缩 A 加 `content-replacement`（批次 1）→ 记忆 C 加提取游标条目（批次 3）。
- `services/compact/auto_compact.rs`：压缩 B（批次 2）、压缩 E（批次 3）→ 记忆 C 的压缩前强制提取（批次 3，与压缩 E 同批
  但须排在其后）。

**先设计、后实现**：四个模块的设计全部完成（第五节待定项清零）并经用户确认后，才开始实现。开工时从**批次 0 +
压缩 A/C/D** 开始：压缩改动最小、收益最大、能最快验证"cache 不断"的核心约束。

## 四、跨模块注意事项（审阅四份设计时发现的问题，已写入各文档）

1. **大 transcript resume 丢掩码**（压缩）：transcript > 5 MB 时 resume 只保留 `METADATA_MARKERS` 中的
   boundary 前条目，其中没有 `content-replacement`；配合"保留尾部原文"会让已掩码结果在 resume 后复活。
   修法：加进列表（压缩切片 A）。
2. **摘要请求形态**（压缩 C6）：沿用 CC 的共享 cache 分叉，只换指令内容；legacy 的序列化纯文本会丢整段 cache。
3. **单独配置模型的代价**（压缩 C10、记忆 M2、外部搜索 4.4）：换模型就不能共享主会话 cache；未配置时一律用主模型。
4. **路径作用域**：记忆按 git root、索引按 worktree、快照全局（见第一节第 6 条）。
5. **压缩可清名单**：新增的 `CodeSearch` 要加进 `is_compactable_tool_name`（`services/compact/micro_compact.rs:507`）；
   WebFetch / WebSearch 已在名单里。WebFetch 改为返回原文后单条结果更大，正是掩码的主要清理对象。
6. **代理与 SSRF**（外部搜索 4.2）：本机所有出站经本地代理 `127.0.0.1:7897`；实测代理会替客户端连回环与 LAN，
   本地 DNS 有污染且代理配置为 fake-ip。设计为"拒绝私网 IP 字面量与私网名字 + 不做本地解析 + 经代理远端解析"，
   残余风险（公网名字远端解析到私网）与代理侧 `REJECT` 的缓解选项已写明。
7. **WebSearch 的 provider 判定**：CC 的 `get_api_provider()` 不看 base URL，网关被当成 first-party；新实现不再依赖
   Anthropic 服务端工具，`is_enabled` 改为"至少配置了一个 provider"。
8. **自建服务不走代理、可降级**（检索 3.1、3.2；外部搜索 3.2.1）：embedding / rerank（corp-pc）、Qdrant / SearXNG
   （nyan-ops）的客户端显式 `no_proxy()`；任一不可达时服务状态机判 Down、退避期间不发请求——corp-pc 关机时 CodeSearch
   用确定性分支、WebSearch 跳过精排；nyan-ops 不可达时 semantic 与 SearXNG 跳过。状态机一份实现（第一节第 9 条），
   检索、外部搜索、Decision Provider 共用。
9. **cache 共享分叉的三条规则**（第一节第 8 条）：不带 Agent-Id、只重放不新增掩码、统一用 `is_cache_sharing_fork`
   判别。压缩 C2 / C6 / C12 / G 与记忆 M2 / M5 都引用这一处。
10. **落盘 usage 只用于事后统计**（压缩 C13-1）：transcript 里的 assistant usage 可能缺 output 与最终 cache 拆分（停在
    `message_start`）；prompt 侧总量与最终值一致（87 个样本实测），所以 measure 不受影响；保活要 cache 拆分，读内存值；
    事后统计只用带 `stop_reason` 的条目。
11. **rerank 是共享组件**（检索 4.9、G0）：Qwen3-Reranker 必须套官方指令模板（实测裸文本区分度差）；代码检索与网页
    精排共用客户端与服务地址，开关各自独立（代码检索默认关、网页精排默认开）。reranker 不能当通用决策器（实测
    元决策问题得分全 < 0.1，见 decision-provider.md 第一节）。
12. **Decision Provider 只在安全集合内选择**（decision-provider.md 第三、四节）：每个决策点先算确定性答案，模型答案
    落在安全集合外或置信度不足即回退；关键路径与确定性路径并行、800 ms 硬上限。**只增不减的点默认 active**（标注、重排、
    续搜、补跑、触发回退、提前提取），**会减少结果的点默认 shadow**（删除候选、跳过提取）；不用它减少 provider 或跳过分支。
    压缩只有一个 shadow 点（掩码清理顺序，不改清理量）。决策点清单以 decision-provider.md 第四节为准，各模块"决策点"小节引用它。
13. **效果优先于源码隐私**（用户 2026-10-27）：决策模型可以看源码、页面正文、对话与工具结果；只遮蔽凭据片段。
14. **工具输出确定性**（tool-io.md T2）：rg 多线程输出顺序不确定（同命令 20 次 20 种输出，实测），与压缩 C3 的字节一致
    重放、prompt cache 前缀稳定冲突；Grep content / count 加 `--sort=path`。CodeSearch exact 同样排序。
15. **完整性尾行是跨模块契约**（tool-io.md T1）：Read / Grep / Glob / CodeSearch / WebFetch 统一以 `(Complete …)` /
    `(Partial … Continue with …)` 结尾；各模块渲染按此格式。

## 五、待定项

2026-10-27 逐项清理：

| 模块 | 原待定 | 结论 | 文档 |
|---|---|---|---|
| 检索 | corp-pc embedding / rerank 的访问方式 | 两机都用：经 EasyTier 固定地址 `10.144.0.11:8090/8091`；Qdrant `10.144.0.3:6333`；不走代理、无 TLS/token；corp-pc 关机时快速判定 Down 并退回 exact / symbol / text | 检索 3.1、3.2 |
| 外部搜索 | `fetchVia` 默认 `proxy` 与残余 SSRF 风险 | 默认 `proxy`；实测代理会连回环与 LAN、本地 DNS 有污染，改为"拒绝私网字面量与私网名字 + 不做本地解析"；残余风险与缓解已写明 | 外部搜索 4.2 |
| 外部搜索 | WebFetch 默认返回 20000 字符 | 保留（≈5k token，低于 50k 持久化阈值、高于掩码 1% 下限），新增 `maxReturnChars` | 外部搜索 4.4 |
| 外部搜索 | provider 选择、召回与候选 | AnySearch、Brave、DeepSeek、Exa、Firecrawl、Tavily、SearXNG（自建）全部接入，search 与 fetch 都接；每家召回 20 → RRF 30 → Qwen3-Reranker 精排 → 8；fetch 由 nyan 先抓、失败按序回退 provider | 外部搜索 2.2、3.2–3.4、4.6 |
| 压缩 | gpt 经网关 cache 基本失效（C13） | 原结论是统计口径错误；实际是网关侧一次断崖（不在 nyan 修）+ 子代理缺 Agent-Id（新增切片 G） | 压缩 C13 |

2026-10-27 设计审查（四个独立审查，对照代码）后的修订：必须修 11 项、跨模块 3 项、应该修约 25 项，已全部写入各文档；
新增 Decision Provider 设计。审查推翻的旧结论：外部搜索 4.2 的"代理只会碰到私网 443"（实测代理可直达 LAN / tailnet 字面量）、
压缩 C4 的"`message_start` usage 不可信"（prompt 侧与最终值一致）。

仍未定、但不阻塞设计（实现切片的第一步实测）：

| 项 | 状态 |
|---|---|
| 检索评测（检索 H） | 后续单独安排；rerank 默认值、融合权重等它出结论 |
| Decision Provider 的真实一致率 | EMRank 已实测（30 问：jev 28/30、clef 29/30；复用连接后 0.40 / 0.65 s；20 候选批量 jev 0.5–0.6 s）；一致率由日志积累 |
| nyan-pc 到 EMRank 的延迟 | 只在 corp-pc 测过；实现时在 nyan-pc 复测 |
| 保活与 thinking 参数是否进入消息级 cache key | 压缩切片 F 第一步实测 |

仓库外基础设施：FlClash 经代理可直达回环 / LAN / tailnet——已报告并由负责方在两台机器上修复（2026-10-27 13:12，7897 入站
私网目标 REJECT，回归已复核）；nyan 的主机判定仍是第一道防线、不依赖该修复（外部搜索 4.2；报告在
`~/.nyan/reports/2026-10-27-flclash-proxy-private-egress.md`，不在仓库）。
