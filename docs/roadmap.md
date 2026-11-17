# 路线图

状态：2026-11-17 定稿。本文记录方向和阶段，不记录日期承诺。

## 目标

让多台机器和多个 coding agent 一起为一个人的开发服务：工作只在常开的开发服务器上运行，桌面和手机都只是同一个会话的窗口，换设备不需要同步。

- nyan 是其中的 agent，继续保留自己的终端界面，可以单独使用。
- 托管会话、连接机器、桌面与手机界面使用 [Paseo](https://github.com/getpaseo/paseo)，不修改它的代码。
- 我们需要而 Paseo 没有的能力，写成 Paseo 插件。
- nyan 通过 ACP（Agent Client Protocol）接入 Paseo。

## 阶段

| 阶段 | 内容 | 验收 |
|---|---|---|
| 0 | 收拾仓库：清理陈旧工作树，修复不稳定测试 | 主干 CI 连续全绿 |
| 1 | nyan 能在 Paseo 里用：ACP 第一批；`deploy/` 下的部署配置 | 在 Paseo 里用 nyan 完成一次带工具调用和审批的对话，换一台设备能接着做 |
| 2 | nyan 在 Paseo 里好用：ACP 第二批 | 流式输出、切换模型和模式、斜杠命令、载入历史会话可用 |
| 3 | 仓库改为 monorepo（nyan 搬进 `agent/`）；第一批插件 | 中途插话和回退可用；会话里能到另一台机器执行命令 |
| 4 | 其余插件：任务视图、待处理列表、总 diff、合并前确认、配置分发、草稿同步 | 逐项验收 |
| 5 | 原生桌面客户端（Rust + GPUI），远期 | 另定 |

## ACP 接入

新增子命令 `nyan acp`，在标准输入输出上说 ACP v1（JSON-RPC 2.0，每行一条消息）。它建在现有的无头路径（`src/cli/print.rs`、`src/query_engine.rs`）上。

- 一个进程只服务一个会话。工作目录、会话 id、模型配置目前是进程级状态；Paseo 也是每个会话启动一个 agent 进程。
- 协议类型使用 `agent-client-protocol-schema`（Apache-2.0），消息收发自己实现，便于用内存管道测试。
- 标准输出只承载协议消息，日志一律走标准错误。

第一批（做完即“能用”）：

1. 骨架：子命令、依赖、`initialize`。
2. 把无头模式的公共函数开放给 ACP 模块，只改可见性。
3. `session/new` 与 `session/prompt`，先整段输出。
4. `session/cancel`。
5. 工具调用与结果（`tool_call`、`tool_call_update`），含改文件时的 diff。
6. 权限请求（`session/request_permission`）：允许一次、始终允许、拒绝。

第二批（做完即“好用”）：

7. 流式输出（`agent_message_chunk`、`agent_thought_chunk`）。
8. 权限模式、模型、思考强度的切换。
9. 斜杠命令（`available_commands_update`）。
10. `session/load` 完整重放历史，以及列出历史会话。
11. 用量与计划。
12. 工具执行进度。

通用 ACP 表达不了的能力（中途插话、回退、子 agent 展示）留到阶段 3，用 nyan 专属的 Paseo 接入插件实现。

## monorepo 目录（阶段 3 起）

```
agent/      nyan
plugins/    Paseo 插件（TypeScript）
deploy/     部署配置
docs/       文档
```

约束：只有 nyan 的发布可以标记为 latest，自更新和安装脚本依赖它。

## 开发方式

- 每个任务一个 PR，范围小到可以完整审阅。
- 实现在独立工作树里完成并通过本地检查后，再推送、开 PR、自动合并，流程见根目录 `AGENTS.md`。
- 不通过削弱测试来换取通过。
