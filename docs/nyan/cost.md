# 费用统计（/cost）

nyan 在请求层统一记账：每个完成的模型请求只记一次，覆盖主对话、子 agent、标题/摘要/记忆提取等小任务、压缩、
cache 续命请求和 side query。记账点是 `services/api/request.rs` 的流式结束处与非流式回退处，以及
`utils/side_query.rs`；实现见 `src/cost_tracker.rs`、`src/utils/model/pricing.rs`。

## 价格来源

1. **模型目录**：按（provider kind → 目录 provider，模型 ID）精确查 `utils/model/model_metadata.json`
   的 `cost`（$/Mtok）。`anthropic`、`openai`、`gemini`、`openrouter` 有目录。
2. **profile 覆盖**：`nyan.models.<profile>.cost` 的 `input` / `output` / `cacheRead` / `cacheWrite`
   逐字段覆盖目录值。
3. **本地 kind**（`vllm`、`ollama`）一律 $0。
4. **查不到价**：该请求费用未知。`/cost` 总数显示为「at least $X」，对应模型/来源行显示 `$?`。

`gateway` 类型（如 CPA）没有目录价，**必须在 profile 里手写 `cost`**（至少 `input` 与 `output`），否则费用未知：

```json
"models": {
  "opus": {
    "provider": "cpa",
    "model": "claude-opus-4-5",
    "cost": { "input": 5, "output": 25, "cacheRead": 0.5, "cacheWrite": 6.25 }
  }
}
```

## 计价规则

- 缺 `cacheRead` / `cacheWrite` 单价时按普通输入价计。
- Claude 缓存写入按时长分开记：5m 写入用 `cacheWrite` 价；**1h 写入按输入价 × 2**（Anthropic 官方 1h 价）。
- 推理 token 已包含在输出 token 内，只显示、不另计价。Gemini 的 `thoughtsTokenCount` 计入输出 token。
- **不支持阶梯价**（如按上下文长度分档的价格）：始终使用目录/profile 的单一价格。
- 不计入：Agent 工具结果的 `<usage>` 尾行（保持 CC 原样）、EMRank / embedding / rerank / 网页搜索。

## 来源分组

`main`（主对话、SDK）、`subagent`（`agent:*`）、`compaction`、`keep-alive`（cache 续命）、`side-query`，
其余 query source 记为 `utility:<source>`（如 `utility:extract_memories`、`utility:agent_summary`）。

## 持久化与 resume

照 Pi：每次请求往**主会话**记录追加一条 `{"type":"usage","usage":{…}}`（子 agent 的请求也写主会话，因为它的费用算在
会话总数里）；改动行数和墙钟时间在退出/切换会话时追加 `{"type":"session-stats"}`（取最后一条）。resume 重放这些条目
重建总数、按模型、按来源的统计。CC 的 `ProjectConfig.lastCost` 等字段已删除。

## 预算

`--max-budget-usd` 读取统一总数，现在会真正生效。费用未知的请求不计入，所以对没配价格的 gateway profile 预算不会触发。
