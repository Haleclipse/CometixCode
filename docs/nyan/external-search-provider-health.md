# 外部搜索补充设计：商业 provider 健康状态

状态：已定稿，未实现（2026-11-02）。独立文档，不并入 `external-search.md`；与其冲突的条款以本文为准，H4 切片负责改掉
冲突处。

已定：实现放独立的 `services/web/provider_health.rs`，不改 `service_health.rs`；3.2 的数值确认可用（常量，不进 settings）。
构建、测试、分支与交付流程按仓库 `AGENTS.md`，本文不重复。

设计来源：对照 konbakuyomu/smartsearch 的 `provider_health.py`、`providers/deepseek.py`。它是 CLI 短进程、逐渠道选路，
架构与本项目相反；只借健康记录这一块。

## 一、问题

| 事实 | 位置 |
|---|---|
| 七家里只有 SearXNG 挂了 `ServiceHealth`；六家商业 provider（AnySearch、Brave、DeepSeek、Exa、Firecrawl、Tavily）没有任何健康状态 | `providers/client.rs:53-54` |
| 每次搜索重建全部 provider（含重读 key 文件），向每家发请求，`join_all` 等最慢的一家 | `search.rs:150,204-225`、`providers/mod.rs:397-412` |
| 单 provider 会话预算在发请求**之前**扣除，失败的调用也占名额 | `search.rs:209`、`budget.rs:165-186` |
| 额度用尽的 provider 被设计为"保持 enabled，每次返回具名 `quota_exhausted`" | `external-search.md:540` |
| fetch 回退链同样不记录 provider 状态 | `fallback.rs:130-186` |

**已观察到的代价**：2026-11-01 smoke 里 brave 每次搜索都是 `quota_exhausted`（`docs/SMOKE-EVAL-2026-11-01.md:31`）——
每次白发一个请求、白占一个 brave 会话名额。

**设计上可能、未观察到**：某家挂起时，每次搜索被拖到 8 s 的 per-provider deadline。没有商业 provider 挂起的实测，
收益按"不白发请求、不占名额"算，延迟收益未量化。

## 二、借鉴与取舍

| smartsearch 做法 | 本文 |
|---|---|
| 硬错误（auth/config）首次即冷却且不探测，窗口 ×4；软错误连续失败才冷却 | 采纳，数值见 3.2 |
| 凭据指纹，换 key 即清冷却 | 采纳（3.5） |
| 页面级 403/404 不计入 provider 健康 | 采纳，与 `errors.rs:27` 冲突，见 3.6 |
| 按（provider，能力）分键，search 成功不清 fetch 的冷却 | 采纳 |
| 全部冷却时放行一个软失败的 provider | 采纳变体（3.3） |
| 状态落盘 | **不采纳**：那是短进程的需要；nyan 常驻 |

## 三、设计

### 3.1 键与归属

- 键 = （provider，能力），能力为 `Search` / `Fetch`；Brave 只有 `Search`。
- 注册表只接受六家商业 provider 的名字。**SearXNG 不进本注册表**，调用点直接绕过，继续用 `service_health.rs`。
- **状态放进程级注册表**，不放 `ProviderClient` 字段：`search.rs:150` 每次搜索都重建 provider。沿用 `budget.rs` 的做法：
  进程级单例 + 仅测试用的 `reset()`。
- Auth 与 QuotaExhausted 是账户级（同一把 key、同一份余额，`external-search.md` 2.2）：发生时同时写入该 provider 的
  `Search` 与 `Fetch` 两个键。其余失败只写发生的那个能力。

### 3.2 失败分级

取值是本提案的，无实测依据；作为 `provider_health.rs` 里的常量。

| 结果 | 级别 | 进入 Down | 窗口 |
|---|---|---|---|
| `Timeout` / `Network` / `Http(5xx)` / `Invalid` | 软 | 连续 2 次 | 30 s 起翻倍，封顶 5 min |
| `RateLimited` | 软 | 第 1 次 | 同上 |
| `Auth`（线上 401；本地无 key，`client.rs:172-177`） | 硬 | 第 1 次 | 10 min 起翻倍，封顶 60 min |
| `QuotaExhausted` | 硬 | 第 1 次 | 同上 |
| `Http(其他 4xx)` | 不计 | — | 请求本身的问题（如 AnySearch 缺 `library` 的 400） |
| `BudgetExhausted` / `Unsupported` / `Unavailable` | 不计 | — | nyan 自己的判定，没有发请求 |
| 200 且零结果 | 成功 | — | 证明可达 |

状态机（每个键）：`soft_streak`、`downs`、`down_until`、`fingerprint`、`probing`。

- 失败：软失败 `soft_streak += 1`，达到阈值（`RateLimited` 为 1，其余为 2）则进入 Down；硬失败直接进入 Down。进入
  Down 时 `downs += 1`，窗口 = `base × 2^(downs-1)`，封顶。
- 成功：`soft_streak = downs = 0`，清 `down_until`。
- 软窗口复用 `service_health::backoff_for`（`service_health.rs:107`）；硬窗口的起点与封顶不同，自己算同样的翻倍公式，
  不要改 `backoff_for`。

### 3.3 Down 期间

- 先查 permit，再 `budget::reserve_provider`：**被跳过的调用不占会话名额**；真实发出去的失败调用仍占。
- 不发请求，但结果末尾仍写具名原因，后缀 `(cooling down)`，例如 `brave quota_exhausted (cooling down)`。原因取自最后一次
  失败，不改写成别的词。实现：`SearchFailure` 加 `cooling_down: bool`，在 `render_failures`（`search.rs:505`）渲染后缀；
  `reason` 仍是原始 code。
- **不变量不动**：没有任何 provider 成功时仍是 `SearchError::Unavailable`（`is_error`），逐家具名，绝不伪装成"没搜到"。
- **共因故障**：六家商业 provider 共用本地代理，代理挂掉会让它们同时超时、同时 Down。因此：当按健康状态本次会跳过
  **全部六家商业 provider**，且它们都处于软 Down 时，放行其中一家照常尝试；硬 Down 的仍跳过。这条规则只看六家商业
  provider，**不把 SearXNG 和 `is_down()` 算进来**（SearXNG 有自己的状态机，这里不读、不调 `call_permit()`）。

### 3.4 恢复：用真实调用试探

商业 provider 的请求要花钱，不复用 `ServiceHealth` 的后台合成探测（那是给免费自建服务的）。窗口到期后，**下一次真实
搜索/抓取就是试探**：成功 → 清零；失败 → `downs += 1`，窗口翻倍。同一个键同一时刻只放行一个试探（会话内最多 2 个并发
搜索，第二个跳过）。

两条实现约束，都来自现有调用点：

1. **permit 是 RAII guard**。调用被取消或 panic 时 guard 被 drop；未显式 record 就 drop，只释放 `probing`，不记成功也
   不记失败。否则一次取消会让该键永远卡在"试探中"。
2. **记录发生在调用点，即 `tokio::time::timeout` 的外面**（`search.rs:215-218`、`fallback.rs:164-170`）。不要放进
   `ProviderClient::search/fetch` 内部：外层 timeout 先到时内层 future 被 drop，里面的记录不会执行。

### 3.5 凭据指纹

指纹 = sha256(base URL ‖ key 内容) 取前 16 个十六进制字符。provider 构建时已经读过 key（`providers/mod.rs:397`），在
`ProviderClient` 上加一个 `fingerprint()` 访问器，经 `ConfiguredSearchProvider` 暴露。每次查 permit 时比对：与键上记录
的不同 → 清除该 provider 全部键的 Down 状态。

- 只存摘要，**不存、不记日志 key 本身**。
- 硬窗口最长 60 min，没有指纹的话用户修好 key 还要干等，所以 3.2 的硬窗口与本条必须一起做。
- 充值（key 没变、余额恢复）不改变指纹，但 **`/clear` 会重置冷却**（`commands/clear/caches.rs` 已在
  `budget::reset()` 旁调用 `provider_health::reset()`），不必等窗口到期或重启 nyan。这条是对原设计"本次不接 `/clear`"
  的修正。

### 3.6 fetch 侧

- `FetchProvider::fetch` 接入同一套 permit/record；`fallback.rs:157-162` 的 `reserve_provider` 之前先查 permit。被跳过的
  provider 记入 `attempts`，原因带 `(cooling down)`。
- **403 的归类是前置问题**：`errors.rs:27` 对所有 provider 把 401/403 一律归为 `Auth`。smartsearch 里 DeepSeek fetch 的
  403/404 表示"页面不可用"，不是 key 问题。如果 fetch 的 403 走硬冷却，一个打不开的页面会让 DeepSeek 下线 60 min。
  - 另外四家（AnySearch、Exa、Firecrawl、Tavily）fetch 遇到打不开的页面返回什么，**没有证据**。
  - smoke 确认之前：**fetch 侧只有 401 与本地无 key 算硬失败，403 / 404 不计入健康**。search 侧 403 仍归 `Auth`。
  - 要求的行为：fetch 能力下，403 与 404 不记入健康；401 与本地无 key 记为硬失败。search 能力下行为不变。
  - 难点：`classify` 把 401 和 403 都映射成 `ProviderError::Auth`（`errors.rs:27`），调用点分不出来；404 本来就是
    `Http(404)`，不受影响。实现机制由实现者选（例如 fetch 路径不经 `classify` 的 403 分支，或让 fetch 调用多带回
    原始状态码），约束只有两条：search 侧的映射与 `errors.rs` 里现有测试 `auth_statuses_are_auth_for_every_provider`
    不能被改弱；所选机制和改动范围在 PR 描述里写明。

## 四、不做

| 项 | 理由 |
|---|---|
| 状态落盘 / 跨进程共享 | nyan 常驻；新进程多花一次失败调用，接受 |
| 解析 `Retry-After` | 只有 Exa 给，30 s 起步的窗口已够 |
| 对商业 provider 发合成探测 | 付费请求 |
| 把健康状态喂进 RRF 权重 | 健康只决定"发不发请求"，不改排序语义 |
| 改 SearXNG 的现有行为 | 它已有状态机和探测，且免费 |
| 排查 `httpstat.us/403` 在 smoke B3 里变成 `network_error`、未触发回退 | 与本文无关，另案（`SMOKE-EVAL-2026-11-01.md:31`） |

## 五、对既有文档的修改（H4）

| 文档 | 条款 | 修改 |
|---|---|---|
| `external-search.md` | 第五节"额度用尽的 provider（如当前的 Tavily）保持 `enabled`：每次返回具名 `quota_exhausted`…" | 改为：保持 `enabled`；首次返回后进入 Down 窗口，窗口内不发请求，仍渲染具名原因；详见本文 |
| `external-search.md` | 3.5 预算 | 注明被跳过的调用不占 `maxCallsPerSession` |
| `README.md` | 第一节第 9 条 | 注明范围：自建服务与 Decision Provider；商业 provider 的健康状态见本文 |

不把本文并入 `external-search.md`；只在上面三处加指向本文的一句话。

## 六、验收

标 **[离线]** 的用 `external-search.md` 第九节的 mock provider；时间相关的用 `_at(now)` 变体（同 `service_health.rs` 的
`call_permit_at` / `record_failure_at`），不真睡；**[smoke]** 需要真实 key。测试不得加 `#[ignore]`。

1. **[离线]** 软失败：mock 第 1 次超时不进 Down；连续第 2 次进 Down；窗口内下一次搜索，该 mock 收到 0 个请求。中间夹一次成功则计数清零。
2. **[离线]** 429 第 1 次即进 Down，窗口 30 s。
3. **[离线]** 402 / 432 / Brave 月度窗口为 0：第 1 次即进 Down 且窗口 10 min；窗口内 0 请求；结果末尾有 `<name> quota_exhausted (cooling down)`。
4. **[离线]** `provider_health` 层：窗口到期后两个并发 permit 只有 1 个放行；试探成功 → 清零；试探失败 → 窗口翻倍。
5. **[离线]** permit 在未 record 时被 drop（模拟取消）后，`probing` 被释放，下一个调用可成为试探；不计成功也不计失败。
6. **[离线]** 外层 timeout 先到（mock 延迟 > deadline）：该 provider 的软失败计数仍然 +1，说明记录在调用点而不在 client 内部。
7. **[离线]** 指纹：改 key 文件内容后，下一次搜索立即向该 mock 发请求，不等窗口；日志与 `Debug` 输出里不出现 key 内容。
8. **[离线]** 所有 provider 都在软 Down：照常尝试；全部硬 Down：`Unavailable`（`is_error`），逐家具名。SearXNG Ready 时不触发放行规则。
9. **[离线]** 200 零结果算成功；400 不计入。
10. **[离线]** `budget::snapshot()` 断言：被跳过的调用不增加该 provider 的计数；发出去的失败调用增加。
11. **[离线]** search 成功不清 fetch 键，反之亦然；Auth / Quota 同时写入两个键。
12. **[离线]** fetch 的 403 / 404 不使 provider 进 Down；fetch 的 401 进硬 Down；search 的 403 仍进硬 Down。
13. **[离线]** SearXNG 现有测试全部通过，行为不变；注册表对 `searxng` 不记录。
14. **[smoke]** AnySearch、Exa、Firecrawl、Tavily、DeepSeek 逐家用"必定 404 的 URL"和"站点封锁的 URL"各 fetch 一次，记录状态码与响应体（响应体先脱敏再入库），据此决定 3.6 的 403 规则能否放宽。

## 七、实现切片

| 切片 | 内容 | 主要写集 | 依赖 |
|---|---|---|---|
| H1 | 注册表、状态机、RAII permit、指纹摘要、`_at(now)` 变体、单测（验收 4、5、7 的摘要部分、9 的状态机部分） | 新 `services/web/provider_health.rs`、`services/web/mod.rs` | — |
| H2 | search 路径：`fingerprint()` 访问器；permit 在 `reserve_provider` 之前；记录在 timeout 外；`cooling_down` 渲染；全跳过放行规则（验收 1–3、6、8–10、13） | `providers/client.rs`（仅加访问器）、`providers/mod.rs`、`search.rs` | H1 |
| H3 | fetch 路径与 403 规则（验收 11、12）。按 3.6 的保守规则实现，不等 smoke；验收 14 只决定以后能否放宽，不阻塞本切片 | `fallback.rs`，必要时 `providers/errors.rs` | H2 |
| H4 | 文档（第五节） | `external-search.md`、`README.md` | H2、H3 |

H2、H3 串行：都经过 `ConfiguredSearchProvider`，且 H3 复用 H2 加的访问器。

## 八、未核实

- 五家 fetch provider 对"打不开的页面"的真实响应（3.6、验收 14）。
- smartsearch 的默认阈值与冷却时长未读到；"空结果不算失败""按能力分键"来自调研子代理的报告，我只亲读了
  `provider_health.py` 文件头和 `deepseek.py` 的 fetch 段。
- 3.2 的软窗口 30 s 对"本地代理短暂抖动"是否合适，没有实测。
- SearXNG 的 `record_failure` 同样写在 `client.rs` 内部（`client.rs:154-159`），外层 timeout 先到时可能不执行。是否真会
  发生取决于两个 timer 的触发顺序，我没有验证；本文不改 SearXNG，但 H2 的验收 6 如果在商业 provider 上复现了这个竞态，
  值得另案查 SearXNG。
