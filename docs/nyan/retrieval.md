# 检索（Retrieval）设计：`code_search`

状态：已定稿，未实现（2026-10-27 审查修订）。两机访问与 corp-pc 不可用时的回退（3.1、3.2）、`worktreeId` 与 collection
命名（4.8）、同步状态与首次同步（4.8.1）、deadline（4.4）已定。

设计来源：nyan-legacy N6（旧 roadmap `88ef45bb0:ROADMAP.md` R1–R20、"Decided architecture" Retrieval 一节）、
Pactile `design/context-governance/retrieval/00–13`（以 01 provider 契约、03 generation、04 Inspect、
05 候选管线、06 降级、10 评测为主）、`.agents/notes/proposed/architecture/2026-10-16-remote-hybrid-code-retrieval.md`、
`deploy/retrieval/README.md`。偏离 CC 的代码用 `@nyan` 标注。

**定位**：与压缩、记忆不同，CC 在这里**没有成熟基座**——只有即时执行的 `rg`（Grep/Glob）和默认关闭的 LSP。
检索是新增能力。legacy 的设计思想（下一节五条）完整保留；它的具体形态（RetrievalPack 词汇、13 项检查表、
远端协调器……）是为 Pactile 的 Kernel / 多客户端 / 评测平面设计的，本项目按自己的结构重新表达（见"九、明确不做"）。检索是本 fork 的重中之重，质量以评测说话（第八节，后续单独安排）。

## 一、设计思想（legacy，不可退让）

1. **确定性分支是下限**：exact、symbol、text 永远可用；semantic 只做参考，不可用时检索照常返回。
2. **候选不是证据**：任何候选——尤其是 semantic——进入结果之前，必须回到当前工作树重新读取，比对原始字节
   hash 与行范围（Inspect）。远端返回的文本永远不直接进模型上下文。
3. **失败不伪装成空结果**：分支超时、不可用、索引构建中、索引过期，各自有名字并让模型看见；"没找到"只在所有
   确定性分支都成功且零命中时才能说。
4. **索引是可重建的派生物，不是事实源**：generation 由参数元组派生，任何影响向量含义的变化都让旧索引失效；
   旧 generation 的候选永不进入结果。
5. **可复算**：同输入、同计划 → 同排序（确定性 tie-break），RRF 的 `k` 与权重显式记录。

## 二、现状（CC）

| 能力 | 现状 | 位置 |
|---|---|---|
| Grep / Glob | 外部 `rg` 即时扫描；Grep 默认 250 条，Glob 100 个 | `tools/grep_tool`、`tools/glob_tool`、`utils/ripgrep.rs` |
| LSP | 9 种操作；**默认关**（`ENABLE_LSP_TOOL`），服务器只能由插件声明 | `tools/lsp_tool/mod.rs:15`、`services/lsp/config.rs` |
| explore 子代理 | 只读、Grep/Glob/Read/Bash；默认模型 `@light` | `tools/agent_tool/built_in/explore_agent.rs`、`utils/model/agent.rs` |
| 索引 / 语义 / BM25 | 无 | — |
| tree-sitter | 已链接 0.25，仅 bash 语法（用于命令安全分析） | `Cargo.toml:105-106` |

本机会话实测：Bash 1086 次（其中 `rg` 485、读片段 `head`/`sed`/`cat` 1000+），Grep 工具 71 次，Explore 1 次。
检索的真实形态是"模型多轮即时 `rg` + 读文件"，每次从零开始。

## 三、外部服务（复用 legacy 部署，corp-pc + nyan-ops）

| 组件 | 位置 | 接口 | 实测 |
|---|---|---|---|
| Embedding `jinaai/jina-code-embeddings-1.5b` | corp-pc，vLLM pooling，fp16（user unit `retrieval-vllm-embed`） | OpenAI 兼容 `POST /v1/embeddings`，`10.144.0.11:8090` | 1536 维、L2 归一、8192 ctx；40.5 chunks/s（1 并发）、91.8（8 并发）；每小时探测全通过 |
| Rerank `Qwen/Qwen3-Reranker-0.6B` | corp-pc，vLLM（`retrieval-vllm-rerank`） | `POST /score`，`10.144.0.11:8091`，返回 [0,1] | 在线；20 候选 p50 350 ms |
| Qdrant 1.19.2 | nyan-ops | `http://10.144.0.3:6333`，需 API key | corp-pc 经 EasyTier 约 0.5 s/请求（首次建连约 1.3 s）；nyan-pc 同网段 |

部署说明与探测脚本在 corp-pc 的 `~/Projects/work/internal/retrieval/`（仅 corp-pc 上有；`probe.sh`、
`retrieval-probes.timer` 每小时一次）。
`retrieval-llama-embed.container` 是保留的 llama.cpp 回退单元，镜像未拉取，处于 failed 状态，不影响 vLLM 服务。

### 3.1 两台机器的访问方式（2026-10-30 更新：EasyTier 固定地址）

nyan 在 **nyan-pc** 与 **corp-pc** 上都要能用 semantic 与 rerank；GPU 服务只在 corp-pc，且 corp-pc 可能关机。
两台机器经 **EasyTier** 互通（不再使用 tailscale），服务以固定内网地址发布：

| 服务 | 地址 | 说明 |
|---|---|---|
| embedding（jina-code-embeddings-1.5b） | `http://10.144.0.11:8090` | corp-pc 上的 vLLM，两机同一地址 |
| rerank（qwen3-reranker-0.6b） | `http://10.144.0.11:8091` | corp-pc 上的 vLLM，两机同一地址 |
| Qdrant 1.19.2 | `http://10.144.0.3:6333` | nyan-ops；`apiKeyFile = ~/.nyan/keys/qdrant` |

- 全部为 **http、无 TLS、无 vLLM token**；Qdrant 仍要 API key（`~/.nyan/keys/qdrant`，两机各一份、0600）。
- 服务只绑定 EasyTier 虚拟网卡地址 / 回环，不暴露到 LAN。
- 两台机器的 settings 用同一组地址（`~/.nyan/settings.json` 仍各机一份、不同步）；nyan 不做自动发现。

**客户端**：

- 检索的三个客户端（embedding、rerank、Qdrant）**一律不走代理**（`reqwest::Client::builder().no_proxy()`），不依赖
  `NO_PROXY` 是否设对。代理会照常替客户端连回环与 EasyTier 地址，绕一圈只是多一跳、多一个故障点。
- 远端 embedding 用 `encoding_format: "base64"`（vLLM 0.32 支持）：单条 1536 维响应从约 33 KB 降到约 8.5 KB。
- 10.144.0.0/24 是 EasyTier 的私有网段，`WebFetch` 的主机判定会拒绝把它当作抓取目标（与检索客户端无关）。

### 3.2 corp-pc 不可用时的回退（已定，2026-10-27）

semantic 与 rerank 是**可选增强**：不可用时，CodeSearch 退回 exact / symbol / text，结果照常返回并注明，不报错、
不卡住。关键是**快速判定不可用**——关机的 corp-pc 不会拒绝连接，只会让连接挂起，不能每次查询都白等一个 deadline。

**服务状态机**：使用公共的 `services/service_health.rs`（README 第一节第 9 条：`Ready ⇄ Down(until)`、退避 30 s 起
翻倍至 5 min、到期后由下一次使用触发一次后台探测、启动不探测；公共切片 H）。本模块不另写状态机，只为三个服务
各注册一个实例，并给出本模块特有的参数：

| 服务 | 连接超时 | 请求超时 | 探测（`Down` 到期后的一次后台请求） |
|---|---|---|---|
| embedding | 1.5 s | 查询侧 2.5 s；同步批次 60 s（4.8.1） | `GET /v1/models`，核对模型 id |
| Qdrant | 1.5 s | 1.5 s | `GET /healthz` |
| rerank | 1.5 s | 2 s | `GET /v1/models`，核对模型 id |

- 连接 1.5 s 覆盖 EasyTier 下首次建连。超时即判 `Down`。
- **Down 期间不发请求**：直接跳过该分支，渲染 `[semantic unavailable (corp-pc unreachable)]`，不计入 deadline。
- 探测只判断"在不在"；readiness 的完整检查（维度、范数等）见 4.8。
- embedding 与 Qdrant 任一 Down，semantic 整体跳过；rerank Down 只跳过重排（排序退回 RRF 融合顺序，不提示）。

**索引同步在 corp-pc 或 nyan-ops 不可用时**：变更文件进入待同步队列，服务恢复后后台补同步（同步状态与队列的
完整定义见 4.8.1）。补同步完成前，这些文件的 semantic 候选会因 hash 不一致在 Inspect 时被拒（4.6），不会返回
过期结果。两台机器各自维护本机索引与各自的 Qdrant collection（collection 名含机器标识，见 4.8），互不干扰。

**写进渲染与验收**：

- `[semantic unavailable (corp-pc unreachable)]`、`[semantic index syncing: N files pending]` 两种说明行（4.2 风格）。
- 验收：corp-pc 关机时，nyan-pc 上 CodeSearch 首次查询最多多等一次连接超时（1.5 s），之后的查询不再等待。
  corp-pc 开机后，恢复由"下一次使用"触发（README 第一节第 9 条）：开机后的查询最多再等一个退避窗口（≤ 5 min）
  即触发探测，探测成功后的下一次查询带上 semantic。期间无人查询则不探测、也不恢复（不做后台轮询）。

## 四、`code_search` 工具

### 4.1 对模型的接口

一个内置工具 `CodeSearch`（`@nyan`），与 Grep/Glob/Read 并列，不替换它们。

```jsonc
{
  "query": "string",              // 必填：自然语言描述、标识符、或字面量
  "path": "string",               // 可选：限定子目录（worktree 相对）
  "glob": "string",               // 可选：文件过滤
  "max_results": 10               // 默认 10，上限 30
}
```

不给模型 `mode` 参数：分支选择是检索服务的职责（4.3 的确定性规则），模型只描述要找什么。要精确字面量时模型
本来就会用 Grep；要符号定义时查询写标识符即可被识别。参数面保持和 Grep/Glob 一样小。

### 4.2 输出

legacy 的 RetrievalPack（`outcome` 枚举、`reason.code`、`fallback.from`……）服务于 Kernel、ABI 与评测平面。
本项目里读结果的是模型，所以**保留它的语义（失败具名、覆盖可见、"没找到"只在全部成功时才说），用 CC 工具结果
的风格表达**——简洁纯文本，和 Grep 的 "No matches found" / "[Showing results with pagination = …]" 同一路数：

```
src/services/compact/auto_compact.rs:198-290  fn auto_compact_if_needed  (exact, symbol, text)
<片段：Inspect 时读到的当前内容，最多 40 行>

src/query.rs:1093-1102  (text, semantic)
<片段>

[searched: exact, symbol, text — semantic unavailable (embedding service unreachable)]
[2 results dropped: file changed since indexing]
```

- 只有出问题时才出现方括号说明行；全部正常时只有结果。
- 零结果：全部分支都成功时写 `No matches found`；有分支失败时写 `No matches found in: exact, symbol (semantic unavailable — results may be incomplete)`。
  这就是 legacy "abstain 只在全部确定性分支成功时成立"的落地方式。
- 分支名对模型用直白的词（`exact` / `symbol` / `text` / `semantic`），不暴露内部的 structural/lexical 术语。
- 结构化结果（每条候选的分支 rank、hash、被拒原因、各分支耗时）**不进 transcript**：检索服务以库接口返回
  `SearchOutcome` 结构体，工具只负责渲染成上面的文本；评测 harness 直接调库接口拿结构体（第八节），不经工具、
  不解析文本。

### 4.3 分支

| 分支 | legacy 对应 | 实现 | 可信度 |
|---|---|---|---|
| `exact` | exact | 复用 CC 的 `rip_grep`（`utils/ripgrep.rs:650`）：查询中的标识符及其 snake/camel 变体；ignore 与 Read deny 规则见下 | 确定性，下限 |
| `symbol` | structural + relation | tree-sitter 符号表（4.5）：按名查定义、文件大纲；引用 / 调用者由同一张表的"标识符出现位置"派生 | 确定性 |
| `text` | lexical | 进程内 BM25（tantivy），按 chunk；分词同时切标识符（`autoCompactIfNeeded` → `auto compact if needed`） | 确定性 |
| `semantic` | semantic | jina embedding（corp-pc）+ Qdrant（nyan-ops），按 chunk | 参考，不可用时不影响其余 |

**exact 分支的权限**：`rip_grep` 本身只执行 `rg`，不带任何 ignore 或权限过滤。Grep 工具是在调用前自己拼
`--glob !<pattern>`：`grep_tool/mod.rs:283-321` 的 `read_deny_globs` 用 `get_file_read_ignore_patterns` 取 Read deny
规则、`normalize_patterns_to_path` 按项目根与（含符号链接解析后的）搜索根展开。exact 分支**复用同一段逻辑**：把
`read_deny_globs` 从 Grep 工具私有函数提出为共享函数（如 `utils/permissions/filesystem.rs` 旁的
`read_deny_rg_globs(permission_context, project_cwd, search_root)`），Grep 与 exact 都调用它；写进切片 B 的写集。
`.gitignore` 由 `rg` 自身处理（与 Grep 同一默认参数）。

选分支的规则（确定性，legacy `RuleDecisionProvider` 的思路）：

- 默认四个分支都跑（并行，开销由 deadline 约束）；
- 查询是单个标识符或路径时，`symbol` 的结果在融合中加权到 2.0（找定义是这类查询的首要意图）；
- 查询含调用 / 依赖信号（`caller`、`who calls`、`调用`、`依赖`、`影响`…）时，`symbol` 额外返回引用位置；
- semantic 未配置时不跑、不提。

以上规则是**确定性默认**，也是 Decision Provider 不可用时的回退；哪些点可交给快速决策模型见 4.11。

### 4.4 执行管线

```
query → 选分支（4.3 的规则）
      → 各分支并行（tokio 任务；共享工具调用的 abort 信号；各自 deadline）
      → 路径规范化（worktree 相对、`/` 分隔、保留大小写；越界即丢弃）
      → 合并同一文件的重叠范围（保留全部来源分支，用于结果后的 "(exact, symbol)" 标注）
      → RRF 融合（只用名次，k 与权重是常量；同分按 路径字节序 → 起始行 → 分支顺序 决胜，保证可复算）
      → [可选] rerank
      → Inspect（4.6）
      → [Decision Provider] retrieval.answers 批量判定 → 重排与标注；retrieval.sufficient → 必要时追加一轮（4.11）
      → 渲染（4.2）
```

默认参数（常量，可在 `nyan.retrieval` 覆盖）：每分支最多 50 个候选；融合后保留 50；返回 `max_results`（默认 10，
上限 30）；每文件最多 3 条；每条片段 40 行；总 deadline 8 s；RRF `k = 60`，各分支等权。

**semantic 的 deadline**（审查修订）：semantic 分支是串行的两步——查询 embedding → Qdrant 检索——**分支 deadline 截断
子请求**：每个子请求有自己的请求超时（3.2 表），分支在 `semanticDeadlineMs` 到达时整体放弃（`TimedOut`），不等子请求
自然超时。默认 `semanticDeadlineMs = 4500`，满足 ≥ embed 请求超时 2.5 s + Qdrant 请求超时 1.5 s + 余量：

| 机器 | embedding（查询，1 条） | Qdrant 检索 | semantic 典型耗时 |
|---|---|---|---|
| corp-pc | ~20 ms（同机） | ~0.5 s（EasyTier 直连 nyan-ops，热连接） | ~0.5 s |
| nyan-pc | 经 EasyTier 到 corp-pc（实现时实测） | 经 EasyTier 到 nyan-ops（实现时实测） | 待实测 |

- 两步共用连接池（keep-alive），首次建连只在首查询付出。
- semantic 与其余分支并行，总 deadline 8 s 不变；semantic 超时不影响 exact / symbol / text 的结果。
- nyan-pc 上的数字需实测；若 p90 超过 4.5 s，只调大 `semanticDeadlineMs`，不改结构。

- 用户按 Esc：沿用 CC 工具的 abort 语义（整个工具调用取消），不另造状态。
- 某分支超时：该分支结果丢弃，其余照常，渲染为 `[semantic timed out]`；超时与取消在内部枚举里是两个值，
  评测能区分（legacy DL-2 的意图），对模型只需要知道"这个分支没结果"。

### 4.5 本机索引（symbol / text / semantic 的数据来源）

- **位置**：`$CLAUDE_CONFIG_DIR/projects/<sanitize_path(worktree 根)>/retrieval/`（`session_storage::get_project_dir`，
  `utils/session_storage.rs:773`；符合 legacy Decision #1 与 03 IX-1：**永不写进工作树**）。
- **按 worktree 区分，不按 git 仓库共用**：memdir 用 canonical git root（`memdir/paths.rs:89-91`），同仓库的所有
  worktree 共用一份记忆；索引不能照抄——不同 worktree 的内容不同，必须各自一份（legacy GEN-6，`worktreeId` 进 generation）。
  worktree 根 = `git rev-parse --show-toplevel`，非 git 目录用 cwd。
- **内容**：每文件 `{path, fileHashRaw(sha256), size, mtime, language}`；tree-sitter 符号表（名称、类型、
  定义行范围、父作用域）；chunk 表（按符号边界切分，带 breadcrumb：`path › impl Foo › fn bar`）；tantivy 索引目录。
- **chunker**：tree-sitter 符号边界优先（函数 / 方法 / impl / class），超过 ~60 行或 2 KiB 正文再按行窗口切分；2 KiB 上限避免生成式 JSON 等密集长行超过 embedding 模型上下文。
  不跨文件（03 CH-2）。语言：Rust、TypeScript/JavaScript、Python、Go、Java、C/C++、Bash、Markdown（标题切分）
  ——首批以本仓库与 nyan-legacy 用到的为准，其余语言退化为行窗口 chunk + 无符号表（这些文件只参与 text / semantic，
  结果标注 `(no symbol check)`）。
- **文件清单**：**不复用** `hooks/file_suggestions.rs`——它以进程 cwd 为键（`file_suggestions.rs:330`，
  `std::env::current_dir()`）、是 typeahead 共用的单例缓存（`FILE_CACHE`，`:30`）、节流刷新（`:301-326`），且结果是相对 cwd
  的路径，不是以 worktree 根为准的完整清单。本模块自己遍历：用 `ignore` crate 的 `WalkBuilder`（`Cargo.toml:99`
  已依赖 `ignore = "0.4.24"`，锁定 0.4.33），以 worktree 根为起点，规则来源为 `.gitignore` / `.ignore` / git 全局
  excludes（`WalkBuilder` 默认行为，与 `rg` 一致），另加 Read deny 规则（与 exact 分支同一个共享函数，见 4.3）
  与 4.5"跳过"规则。git 与非 git 目录同一条路径，不分两套。
- **变更检测**：有 git 时用 `git status --porcelain` + 上次索引时的 HEAD 与当前 HEAD 的 `git diff --name-only`
  得到变更文件集合，只对这些文件比 mtime/size、再算 sha256；无 git 时对清单做 mtime/size 预筛。68 万行的本仓库
  每次检索前全量 stat 太重，git 已经知道哪些文件变了。
- **何时更新**：会话启动时后台一次；之后在每次 `CodeSearch` 前做一次上述轻量变更检测（通常只有几个文件），
  以及 Inspect 发现 hash 不一致时把该文件入队。只重建变更文件的符号、chunk 与倒排条目；删除文件移除其条目。
  不做文件监听。
- **跳过**：二进制、> 1 MB、`.env`/私钥类路径、权限 deny 规则命中的路径 → 记入 `skippedFiles`（分类计数，不静默）。
- **首次建索引**：后台任务；未就绪时 symbol / text 跳过，exact 照常返回（legacy bootstrap 规则，见 4.10）。
- **依赖核实（2026-10-26，crates.io）**：语法 crate（`tree-sitter-rust` 0.24.2、`-typescript` 0.23.2、`-python` 0.25.0、
  `-go` 0.25.0、`-java` 0.23.5、`-cpp` 0.23.4、`-md` 0.5.4）只依赖 `tree-sitter-language ^0.1`（本仓库已有 0.1.7），
  与已链接的 tree-sitter 0.25 ABI 兼容；`tantivy` 0.27.0，MIT，`rust-version` 1.88，低于本 crate 的 1.99，兼容。
  许可均与 AGPL-3.0 兼容。
- **构建代价（决定）**：语法 crate 每个是 0.4–1.0 MB 的生成 C 源码（crates.io 包体：rust 0.4 MB、typescript 0.8 MB、
  cpp 1.0 MB），编译成静态对象并链接进二进制；`tantivy` 0.27 有 42 个非可选的普通依赖。本机尚无构建产物，**不预估数字**：
  切片 C 与 E 的第一步各自测量 release 构建的增量（二进制体积、`cargo build --release` 冷编译时间，与改动前对比），
  写进切片 commit message。阈值：二进制增量 > 15 MB 或冷编译时间增量 > 25%，则该部分改为 cargo feature
  （`retrieval-grammars` / `retrieval-text`，默认开启，可在构建时关掉），对应分支在关闭时按"未启用"处理（4.10）。

  **实测（2026-10-29，切片 C + E 完成时）**：`cargo build --release --bin nyan`，`RUSTC_WRAPPER=`（关 sccache）、
  独立空 `CARGO_TARGET_DIR`，同一台机器背靠背跑。基线 = `git archive HEAD` 原 Cargo.lock `--locked`。未引用的 crate
  会被链接器丢掉，所以测量时全部语法 crate 已从 `grammar_for` 引用、tantivy 从 text index 引用。

  | 指标 | 基线 | 语法 + tantivy | 增量 |
  |---|---|---|---|
  | 二进制 | 56,511,240 B | 70,145,624 B | +13.6 MB（+24.1%） |
  | 冷编译 wall | 306 s | 341 s | +11.3% |
  | 冷编译 user CPU | 2385 s | 2813 s | +18.0% |
  | 二进制拆分 | — | 语法 +10.2 MB（7 个 crate，+18.1%）；tantivy +3.4 MB | |

  两项阈值均未触发（二进制 ≤ 15 MB、冷编译 ≤ 25%），**不引入 cargo feature**。二进制距阈值仅剩约 1.4 MB，
  再加语法（例如更多语言）前需重新测量。带 sccache 或未引用语法 crate 的构建数字不可比（未引用时二进制只多 1,920 B），
  不采用。

### 4.6 Inspect：候选进入结果前重读当前文件

legacy 的核心要求只有一句：**结果里的每一行内容都必须是此刻工作树里的真实内容**。在本项目里它就是结果渲染前的
一个函数 `inspect(candidate) -> Result<Verified, Dropped>`，不需要 13 项检查表：

1. 用 CC 现有的 Read 权限判定（与 Read 工具同一套 deny 规则与路径规范化）确认可读、在 worktree 内；
2. 读文件，算原始字节 sha256，与候选记录的 hash 比较；
3. 行范围仍在文件内；候选带符号名时，用同一份 tree-sitter 解析确认该符号仍在该范围；
4. 通过 → 片段取自刚读到的字节（不取索引里的任何文本，索引本来也不存正文）；
   不通过 → 丢弃，计入"N results dropped: <原因>"，并把该文件放进增量重建队列。

- 不自动重定位行号：hash 不一致的候选直接丢弃，由更新后的索引在下一次检索中给出新位置（legacy RA-1 的意图）。
- exact 分支的结果本来就是刚扫描的当前内容，直接视为已 Inspect；Inspect 主要拦截 symbol / text / semantic
  这些来自索引的候选。
- 语言不支持符号解析时，候选只按文件 hash 与行范围验证，结果标注 `(no symbol check)`，不假装验证过。

### 4.7 generation（legacy 03 §1，按本机落地裁剪）

```
semanticGeneration = "semgen-" + sha256(canonical_json({
  embeddingModelId, embeddingDim, normalize, precision,
  taskPrefix{query, document}, queryPreprocessVersion, documentPreprocessVersion,
  chunkerVersion, indexSchemaVersion, worktreeId
}))[..32]
```

- 任一字段变化 → 新 generation，后台重建新 collection；旧 collection 停止服务（GEN-1），其候选一律丢弃。
- 不含 `sourceSnapshotHash`：源码变化由文件级 hash 增量同步处理（不整体换代），Inspect 的 file_hash_raw 兜底。
  这是对 03 GEN-1 的有意裁剪：legacy 自己在 remote-hybrid 笔记 §5 也改为"按文件 hash 增量同步"。
- 维度不符（服务返回非 1536、或配置的 Matryoshka 维度与 collection 不一致）→ `wrong_dimension`，整批丢弃，
  需重建（EMB-2）。
- text 索引（tantivy）同理，以 analyzer + schema + chunkerVersion 派生版本号，变化即重建。

### 4.8 semantic 分支细节

- **embedding**：`POST /v1/embeddings`；文档侧不加前缀，查询侧加 jina `nl2code` 查询前缀
  （`Find the most relevant code snippet given the following query:\n`，与部署 smoke 测试一致）；前缀字符串进 generation。
- **readiness**（legacy 09，"HTTP 200 ≠ ready"）：首次使用与每 10 分钟探测一次——`/v1/models` 的 id、单条向量维度
  1536、有限值、L2 范数≈1、批量对齐。任一不符 → 该 provider `unsupported` 或 `provider_unavailable`，本次不服务。
- **响应校验**（EMB-1）：条数不符、维度不符、出现 NaN/Inf → 整批 `invalid_response`，不部分接受、不当低分。
- **Qdrant**：每个 `(worktreeId, semanticGeneration)` 一个 collection；payload `{path, startLine, endLine, chunkId,
  fileHashRaw, symbol?}`，只存 hash 与范围、**不存正文**（03 CH-3，同时满足隐私：正文不留在 nyan-ops）。
- **`worktreeId`**（审查修订）：两台机器共用 nyan-ops 上同一个 Qdrant，collection 名必须跨机器唯一。不能靠
  `sanitize_path`（`session_storage.rs:725`，只把非字母数字换成 `-`）——两机路径不同（`/home/yuki/…` 与 `/home/blxze/…`）
  只是巧合，同用户名同路径的两台机器就会撞名。定义：

  ```
  machineId  = sha256("/etc/machine-id" 的内容，去掉末尾换行)[..12]   (hex)
  worktreeId = machineId + "-" + sha256(worktree 根的规范路径，realpath + NFC)[..16]
  ```

  - 机器标识取 `/etc/machine-id` 而不是主机名：前者由 systemd 生成、全局唯一、机器在线离线与改名都不影响；
    只取 hash，不把原值发往 nyan-ops（`machine-id` 本身视为不应外发的标识）。
    非 Linux 或读取失败时退回 `sha256(hostname)` 并在索引元数据里标注 `machineIdSource = "hostname"`。
  - worktree 根取 `git rev-parse --show-toplevel` 的 realpath（4.5），同一机器上不同 worktree 必然不同。
- **collection 名**：`nyan_<worktreeId>_<semanticGeneration 的前 16 位 hex>`，例：
  `nyan_3f9a1c0b7d2e-8c41f0aa93b25e6d_a1b2c3d4e5f60718`（63 字符）。只含 `[a-z0-9_-]`，满足 Qdrant 的限制
  （核实 qdrant/qdrant master `lib/common/common/src/validation.rs`：不得含 `< > : " / \ | ? * \0 \x1F`、不能是 `.` / `..`；
  `lib/api/build.rs`：长度 1–255）。人类可读的对照（机器名、worktree 路径、generation 全文）只存在本机索引元数据里，
  不进 collection 名或 payload。
- **nyan-ops 上的 collection 清理**：
  - generation 被替换：新 collection 构建完成并切换后，**删除同一 `worktreeId` 的旧 collection**（只删本机 `machineId`
    前缀下、本机元数据记录过的名字，不按通配删别的机器的）。
  - worktree 删除或长期不用：**不自动删**（无法区分"暂时没打开"与"不再需要"）。提供手动命令
    `/retrieval prune [--dry-run]`：列出本机 `machineId` 前缀下、本机索引目录已不存在或超过 90 天未同步的 collection，
    确认后删除。另一台机器的 collection 只能在那台机器上清理。
- **同步**：本机索引增量扫描产生变更 chunk → 批量（32）embedding → upsert / delete；同步状态、失败处理与首次全量同步
  见 4.8.1。
- **egress**：源码 chunk 正文只发往用户自己的 embedding 服务（两机都经 EasyTier 虚拟网络到 corp-pc，
  不经第三方）；发往 nyan-ops 的是向量 + 路径/行号/hash。按 legacy
  Decision #8"配置即授权"：在 `settings.nyan.retrieval.semantic` 里配置了服务即视为授权；未配置不发任何请求。
  `.env`、私钥、凭据类路径在索引阶段就跳过（4.5），永不外发。
- **单写者**：多个 nyan 进程同时打开同一项目时，用索引目录下的文件锁保证只有一个进程做同步；其它进程只读查询。
  不做 legacy 的远端协调器 / epoch / CAS（见第九节）。

### 4.8.1 同步状态、待同步队列与首次全量同步（审查修订）

**一套状态**（替代原先分散的 `pending.jsonl` 与 `index_partial` 两种说法）：

```
SemanticSyncState = Synced
                  | Pending { files: n, reason: ServiceDown | BatchFailed | InitialSync }
                  | Rebuilding                       // generation 变更，新 collection 构建中（4.7）
```

- **待同步队列**是唯一的进度记录：索引目录下 `semantic-pending.jsonl`，每行 `{path, fileHashRaw}`（不存正文）。
  - 本机索引发现文件变更 → 入队；
  - 批次成功（embedding 全部校验通过且 Qdrant upsert 返回成功）→ 该批文件出队；
  - 批次失败（embedding 或 Qdrant 报错、超时、`invalid_response`）→ **该批文件留在队列**，`reason = BatchFailed`，
    不部分提交；同一批连续失败 3 次则该服务判 `Down`（交给状态机退避），不在本地空转重试。
  - 服务 `Down` → 不尝试同步，新变更照常入队，`reason = ServiceDown`。
- **状态由队列推导**：队列空 → `Synced`；非空 → `Pending { files: 队列中的文件数 }`。不另存状态字段，重启后读队列即恢复。
- **渲染**：`Pending` 时 `[semantic index syncing: N files pending]`（N = 队列文件数）；`Rebuilding` 时
  `[semantic index rebuilding]`。两者都不阻止 semantic 分支对已同步部分返回结果；未同步文件的候选在 Inspect 时因
  hash 不一致被拒（4.6）。内部分支结果枚举相应增加 `Partial { pending }`（4.10）。

**首次全量同步的规模（本仓库估算，2026-10-27）**：

- 本仓库 `git ls-files` 中 Rust 1438 个文件、约 68 万行（首批语言合计约 68 万行，几乎全是 Rust）。按 4.5 的切分规则
  （符号边界，超过 60 行再按窗口切）粗估 **约 3.3 万个 chunk**，平均约 200 token / chunk。
- **corp-pc**：本机 embedding 是快路径；同步默认把跨文件 chunk 合并为 128 条一批、8 批并发，避免小文件各自支付一次 Qdrant 往返。
- **nyan-pc**：embedding 经 EasyTier 到 corp-pc，Qdrant upsert 再走一次 EasyTier 到 nyan-ops。2026-10-31 实测 Qdrant `/healthz`
  约 0.72 s、128 点 upsert 约 1.7–2.5 s；同步 embedding 批次使用独立 60 s 超时，Qdrant 批次使用独立 15 s 超时，不再受查询侧 1.5 s 超时截断。当前仓库约 32919
  chunks；本次按 2 KiB chunk 上限实际生成 36122 chunks，按 128 点批约 283 次 upsert。8 并发后瓶颈从请求次数降为 embedding 服务吞吐，实际同步可跨多个 checkpoint 窗口完成。
- **取舍（决定）**：接受耗时，不限制只在直连时同步——首次同步在后台进行，期间 semantic 对已同步部分照常可用、
  其余分支不受影响，模型看到 `[semantic index syncing: N files pending]`；中断（退出、关机、网络断）后重启从队列续传，
  已完成的批次不重做。若实测 nyan-pc 上超过 30 分钟，再考虑由 corp-pc 代为计算向量（需 corp-pc 上有相同 worktree，
  本设计不做）。

**首次同步的规则**：

- 触发：本机索引首次构建完成后（symbol / text 已就绪），且 semantic 已配置；把全部可索引文件入队，`reason = InitialSync`。
- **前提**：embedding 与 Qdrant 均为 `Ready`（第一次使用时探测）；任一 `Down` 则首次同步不开始，队列保持，
  状态 `Pending { reason: ServiceDown }`，等服务恢复由下一次使用触发。
- 并发度：默认同时在途 8 个批次；每批 128 条。embedding 同步请求超时 60 s，Qdrant 同步批次超时 15 s，
  两者都与查询侧 2.5 s / 1.5 s 分开。批次跨文件合并，但只有该文件贡献的所有批次成功后才会从队列移除。
- 后台任务：不阻塞会话，不阻塞 CodeSearch；用户按 Esc 只取消当前工具调用，不取消后台同步。同步队列在每个文件窗口完成后
  原子 checkpoint；进程退出后从最近窗口继续。需要等待整轮完成时使用 `/retrieval sync --wait`，否则 `/retrieval sync` 只启动后台任务。
- 单写者：同步持有 4.8 的文件锁；第二个进程只读查询，不同步。

### 4.9 rerank（可开关，交给评测决定）

- legacy 记录"reranker 评测负收益、默认关闭"，但仓库与 corp-pc 上都**找不到这份评测的数据**；按 legacy 评测原则
  （不得引用无法复现的旧数字），这只能作为待验证假设。
- **请求必须套 Qwen3-Reranker 的官方指令模板**（2026-10-27 实测，同一组 相关 / 无关 / 半相关 文档）：
  裸文本 `text_1=query, text_2=docs` 得分 0.878 / 0.145 / 0.836（区分度差，半相关几乎与相关同分）；套模板后
  0.999 / 0.000 / 0.010。部署的 `probe.sh` 与 `bench-rerank.py` 都用裸文本——legacy 的"负收益"很可能就是没套模板。
  模板（`/score` 的 `text_1` / `text_2`）：

  ```
  text_1 = "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n<Instruct>: {instruct}\n<Query>: {query}\n"
  text_2 = "<Document>: {doc}<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
  ```

  `instruct`：代码检索用 `Given a code search query, retrieve relevant code snippets that answer the query`；网页候选
  （外部搜索 3.4）用 `Given a web search query, retrieve relevant passages that answer the query`。模板字符串进
  generation（换模板即换 rerank 行为，评测可区分）。20 个候选套模板后 133 ms（本机）。
- 设计：`settings.nyan.retrieval.rerank.enabled = false`（默认关），开启时对融合后 top-N（默认 10，2026-11-01 评测：Hit@5 与 MRR 优于 20，延迟约为 1/4）调一次 `/score`，
  只用于重排；分数必须在 [0,1] 且有限，否则整批作废、回退到融合顺序。送去 `/score` 的片段按字符截断
  （`maxDocumentChars`，默认 3000；按最坏 1 token/字符，给模板和查询留出余量，避免单篇 `Cargo.lock` 打出 vLLM 400）。截断不影响返回给模型的片段。
- 是否默认开启，由评测决定（第八节）。
- **rerank 客户端是共享组件**：模板、`/score` 调用、分数校验放在 `services/rerank.rs`（切片 G0），服务健康用公共
  `services/service_health.rs` 的 rerank 实例（README 第一节第 9 条），代码检索与外部搜索精排（外部搜索 3.3）共用；
  服务地址只在 `nyan.retrieval.rerank.url` 配一处。**开关各自独立**：
  `nyan.retrieval.rerank.enabled`（代码检索，默认关，等评测）与 `nyan.web.search.rerank.enabled`（网页精排，默认开）。

### 4.10 降级（legacy 06 的语义，按本项目表达）

内部用一个枚举表达每个分支的结果：`Ok | Partial { pending } | Unavailable(reason) | Building | Rebuilding | TimedOut |
Cancelled | Stale`（`Partial` / `Rebuilding` 只用于 semantic，由 4.8.1 的同步状态映射），渲染时转成 4.2 的方括号说明行。规则：

| 情况 | 行为 | 模型看到的 |
|---|---|---|
| semantic 未配置 | 不跑、不提 | 什么都不提（未启用 ≠ 降级） |
| embedding / Qdrant 不可用 | 跳过 semantic，其余照常；进入 Down 退避，期间不再发请求（3.2） | `[semantic unavailable (…)]` |
| 待同步队列非空（首次同步中、服务曾 Down、批次失败；4.8.1） | 已同步部分照常（`Partial`）；未同步文件的 semantic 候选在 Inspect 被拒 | `[semantic index syncing: N files pending]` |
| 本机索引还在首次构建 | symbol / text 跳过，exact 照常 | `[symbol, text index building — exact only]` |
| 模型参数变了（generation 不符 / 维度不符） | semantic 候选全部丢弃，后台重建（`Rebuilding`） | `[semantic index rebuilding]` |
| semantic 分支超过 `semanticDeadlineMs` | 丢弃该分支，其余照常（`TimedOut`） | `[semantic timed out]` |
| rerank 失败 | 用融合顺序 | 不提（排序质量问题，不影响结果真实性） |
| 所有分支都失败 | 工具返回错误（`is_error`），不返回空结果 | `Search failed: …` |
| 全部成功且零命中 | — | `No matches found` |

### 4.11 决策点（Decision Provider 接缝）

本模块里可能交给快速决策模型的点如下。**是否接入、接口、超时与评测由 [decision-provider.md](decision-provider.md)
决定**；本文只列出决策点与确定性回退——Decision Provider 未配置、不可用或超时时，一律按回退执行，结果与没有
Decision Provider 时完全相同。

| 决策点 | 确定性回退（现行规则） | Decision Provider（decision-provider.md 4.1） |
|---|---|---|
| 查询意图 → symbol 加权、是否返回调用者 | 4.3 关键词规则 | `retrieval.intent`（active）：只影响权重 1.0 / 2.0 与是否返回调用者 |
| 候选是否回答查询 | RRF / rerank 顺序 | `retrieval.answers`（active）：Inspect 后对候选批量判定，回答查询的提到最前并标注；不删除 |
| 结果是否已足够 | 不续搜 | `retrieval.sufficient`（active）：判"不足"时追加一轮（补跑 semantic 或扩大 `max_results`），最多 1 轮 |
| 删除无关候选 | 不删 | `retrieval.prune`（shadow）：P(answers) < 0.05 才删，至少留 3 条 |
| 查询改写 / 扩展 | 只做 snake / camel 变体 | 不接入（需要生成文本） |
| 是否跳过某个分支 | 四路全跑 | 不接入（效果优先，只增不减） |
| 是否调用 rerank | `rerank.enabled` | 不接入（由检索评测定默认值） |

约束（与本模块设计思想一致，第一节）：决策只影响**排序、标注、symbol 加权、是否追加一轮**，不影响 Inspect 与
"没找到"的判定。三个确定性分支（exact / symbol / text）**始终运行**——Decision Provider 的安全集合不含"跳过确定性分支"，
所以"没找到只在所有确定性分支都成功时才能说"的规则不受影响。排序可复算：决策输出记入 `SearchOutcome`（库接口），
评测回放时重用记录的决策而不重新请求（decision-provider.md 第三节"可复算"）。

## 五、与其它模块的关系

- **记忆 M6**：检索服务是一个库（`services/retrieval`），`CodeSearch` 工具只是它的一个调用方。记忆模块直接调用
  库接口检索 memdir（memdir 单独建一个小索引：只有 text + semantic，按 git root），命中项用 CC 现成的
  `Attachment::RelevantMemories` 注入——走 CC 的 attachment 管线，不经工具调用。只在记忆条数超过 MEMORY.md
  索引能容纳的规模时启用（记忆少时 MEMORY.md 已全量注入）。
- **压缩**：`CodeSearch` 加入 `is_compactable_tool_name`（`services/compact/micro_compact.rs:507`，可被掩码清理），由切片 I
  完成；README 第四节第 5 条有同样的记载（`compaction.md` 本身未列出 `CodeSearch`，以 README 与本切片为准）。
- **外部搜索**：不在本工具内；`WebSearch` / `WebFetch` 单独设计（Search → Fetch → Evidence）。两处共享：rerank 客户端
  （切片 G0，`services/rerank.rs`；外部搜索切片 F 依赖它）与服务健康状态机（公共切片 H，`services/service_health.rs`）。
  rerank 的服务地址只配在 `nyan.retrieval.rerank.url`，开关各自独立（4.9）。
- **Decision Provider**：本模块的决策点与回退见 4.11。
- explore 子代理由用户后续统一调整。

## 六、配置

`settings.nyan.retrieval`：

```jsonc
{
  "enabled": true,
  "index": { "maxFileBytes": 1048576, "languages": "auto" },
  "semantic": {
    // 两台机器相同，经 EasyTier（3.1）
    "embedding": { "url": "http://10.144.0.11:8090", "model": "jina-code-embeddings-1.5b",
                   "dim": 1536, "apiKeyFile": null },
    "qdrant":    { "url": "http://10.144.0.3:6333", "apiKeyFile": "~/.nyan/keys/qdrant" }
  },
  "rerank": { "enabled": false, "url": "http://10.144.0.11:8091", "model": "qwen3-reranker-0.6b", "topN": 10, "maxDocumentChars": 3000 },
  "budget": { "deadlineMs": 8000, "semanticDeadlineMs": 4500, "rrfK": 60,
              "connectTimeoutMs": 1500, "embedTimeoutMs": 2500, "qdrantTimeoutMs": 1500, "rerankTimeoutMs": 2000 },
  "sync": { "batchSize": 128, "concurrency": 8, "embedBatchTimeoutMs": 60000,
            "qdrantBatchTimeoutMs": 15000, "maxBatchFailures": 3, "pruneAfterDays": 90 }
}
```

退避参数（30 s 起、上限 5 min）属于公共状态机（README 第一节第 9 条），不在本模块配置。

凭据只按文件引用（legacy Decision #4），不写进 settings 正文；未配置 `semantic` = 该分支不启用、不发请求。
`apiKeyFile` 为 `null` 时不发 `Authorization` 头（EasyTier 内网不鉴权，见 3.1）。Qdrant 的 key 首次配置时从
corp-pc 的 `~/Projects/work/internal/retrieval/keys/qdrant-api-key` 复制到各机的 `~/.nyan/keys/qdrant`（0600；
两台机器各一份，README 第一节第 7 条）。

## 七、实现切片

| 切片 | 内容 | 主要写集 | 依赖 |
|---|---|---|---|
| A | 库接口与结果模型：`SearchRequest` / `SearchOutcome`、分支结果枚举、确定性 RRF、路径规范化、Inspect、文本渲染 | 新 `services/retrieval/{mod,outcome,fusion,inspect,render}.rs` | — |
| B | exact 分支（复用 `rip_grep`）+ `CodeSearch` 工具壳 + 4.3 的确定性规则；把 Grep 的 `read_deny_globs` 提为共享函数（4.3） | 新 `services/retrieval/exact.rs`、`tools/code_search_tool/`、`tools/mod.rs`、`tools/grep_tool/mod.rs`（改为调用共享函数）、`utils/permissions/filesystem.rs`（共享函数） | A |
| C | 本机索引：`ignore::WalkBuilder` 遍历、文件 hash、增量、跳过分类、文件锁；tree-sitter 语法与符号表；chunker；第一步测量构建代价（4.5） | 新 `services/retrieval/index/`、`Cargo.toml`（语法 crates） | — |
| D | symbol 分支（定义、大纲、引用） | `services/retrieval/symbol.rs` | A、C |
| E | text 分支（tantivy）；第一步测量构建代价（4.5） | `services/retrieval/text.rs`、`Cargo.toml` | A、C |
| F | semantic：embedding / Qdrant 客户端（`no_proxy`、base64、连接 / 请求超时、分支 deadline 截断）、readiness、generation、`worktreeId` 与 collection 命名、同步状态与待同步队列、首次全量同步、generation 替换后删旧 collection、`/retrieval prune`、校验 | `services/retrieval/semantic/` | A、C、公共 H |
| G0 | rerank 共享客户端：Qwen3 指令模板、`/score`、分数校验 | 新 `services/rerank.rs` | 公共 H |
| G | 代码检索接入 rerank（开关，默认关） | `services/retrieval/` 融合后调用 G0 | F、G0 |
| H | 评测（后续单独安排，见第八节） | — | — |
| I | 接入：压缩可清名单（`micro_compact.rs:507`）、记忆 M6、后台子代理工具白名单（`is_async_agent_allowed_tool`，`agent_tool_utils.rs:224-243`，否则后台运行的子代理拿不到 `CodeSearch`） | 对应文件 | B、F |

"公共 H" 是 README 第一节第 9 条的服务健康状态机（`services/service_health.rs`），不属于本模块的写集。

部署：两机 EasyTier 已就绪，服务地址见 3.1；不再需要额外的发布步骤。

## 八、评测（后续单独安排）

评测是检索质量的唯一判据（legacy Decision #43），具体方案后续单独设计。本设计为它预留的接缝：

- 检索服务的库接口返回完整的 `SearchOutcome`（每条候选的来源分支、名次、hash、丢弃原因，各分支状态与耗时），
  评测 harness 直接调用，不经工具、不解析文本；
- 分支可单独开关（`exact` / `symbol` / `text` / `semantic` / rerank），以便做 legacy 的对照臂（仅 rg、确定性分支、
  加 semantic、再加 rerank）；
- 排序可复算（确定性 tie-break），同输入多次运行结果一致；
- rerank 是否默认开、融合权重、分支选择规则，都等评测结果再定，设计里只给可调的默认值。

## 九、明确不做（相对 legacy）

| 项 | 理由 |
|---|---|
| 远端索引协调器（active-manifest CAS、coordinator epoch/fencing、job lease、Postgres 元数据） | 单用户、单机索引；一个文件锁足以保证单写者。legacy 的协调器为多客户端共享远端索引设计，且从未实现 |
| Tantivy 远端服务 | text 用进程内 tantivy，本机索引即可 |
| legacy Inspect 13 项检查表（含 worktree / branch / source_revision / operation_epoch / snapshot） | 本项目用"重读文件 + 比 hash + 比范围 + 比符号"一个函数表达同一要求（4.6）；单 worktree、无 operation epoch |
| ABI V3 ref / `unconvertible` | Pactile Kernel 的引用语法，nyan 无此边界 |
| DecisionProvider / Jev 规划 | 见 [decision-provider.md](decision-provider.md)（默认确定性规则，Decision Provider 可接管 4.3 的分支选择与权重；决策点见 4.11） |
| RetrievalPack 词汇、reason.code 封闭枚举、ContextUnit / durable 适配 | 结果以 CC 工具结果的文本风格给模型（4.2），结构化数据留在库接口里；进上下文后由压缩模块负责清理 |
| 评测平面与产品平面的 receipt / Langfuse 接入 | 评测结果写本地文件；Langfuse 可后续接入 |

## 十、待定与已定

- 已定：首批语言为 Rust、TS/JS、Python、Go、Java、C/C++、Bash、Markdown，后续按需添加；评测后续单独安排。
- 已定（2026-10-30 更新）：nyan-pc 与 corp-pc 都用 semantic / rerank，经 EasyTier 固定地址（`10.144.0.11:8090/8091`）；
  Qdrant 在 `10.144.0.3:6333`；客户端不走代理、无 TLS/vLLM token，Qdrant 用 key 文件；corp-pc 不可用时快速判定并退回
  exact / symbol / text（3.1、3.2）。
- 已定（2026-10-27 审查修订）：服务状态机用公共组件（3.2）；`worktreeId` 含机器标识，collection 命名与清理（4.8）；
  同步状态由待同步队列推导、首次全量同步后台续传（4.8.1）；semantic deadline 4.5 s 截断子请求（4.4）；文件清单用
  `ignore::WalkBuilder`（4.5）；exact 复用 Grep 的 Read deny 拼接（4.3）；构建代价在切片 C / E 第一步实测（4.5）。
