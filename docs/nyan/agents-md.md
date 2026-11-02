# AGENTS.md 指令文件规范

状态：已实现（2026-11-02）。

## 一、背景与动机

nyan 是 provider 无关的通用 Agent，不应与特定厂商或特定工具（如 Claude Code）的私有约定深度绑定。此前系统沿袭了 Claude Code 的指令发现逻辑，主要读取 `CLAUDE.md`、`CLAUDE.local.md`、`.claude/CLAUDE.md` 以及 rules 规则目录，仅将 `AGENTS.md` 作为补充。

为了实现 provider 无关性，nyan 彻底移除了所有 Claude 专属的指令文件约定，统一使用跨模型与多工具通用的 `AGENTS.md` 作为唯一认可的项目与用户指令规范。

## 二、行为变更清单

1. **仅读取 AGENTS.md**：
   - 彻底不再读取 `CLAUDE.md`、`CLAUDE.local.md`、`.claude/CLAUDE.md`、`~/.nyan/CLAUDE.md`。
   - 彻底废除并停止扫描 rules 目录（包括 `.claude/rules/` 与全局配置目录下的 `rules/`）。
2. **不提供兼容回退与迁移提示**：
   - 若项目中仅存在 `CLAUDE.md`，nyan 会直接忽略它，不会自动回退读取，也不会主动向用户提示迁移。
3. **保留层级发现与 @import 机制**：
   - **Managed 作用域**：`<managed_path>/AGENTS.md`。
   - **UserGlobal 作用域**：`$CLAUDE_CONFIG_DIR/AGENTS.md`（默认 `~/.nyan/AGENTS.md`）。
   - **Project 作用域**：从文件系统根目录向下至当前工作目录（CWD），逐层祖先目录遍历加载 `AGENTS.md`。
   - **Nested 作用域**：嵌套子目录内的 `AGENTS.md` 在按需加载时装载。
   - **@import 语法**：`AGENTS.md` 中依然支持 `@path/to/file` 引用外部文档，支持相对路径解析、跨目录包含安全门禁与深度限制（上限 5 层）。
4. **命令与交互更新**：
   - `/init` 命令：分析代码库并生成标准 `AGENTS.md` 文件。
   - `/memory` 命令：文件编辑与查看菜单默认选项直接指向项目根目录与全局配置目录下的 `AGENTS.md`。
   - **UI 与系统提示词清理**：系统提示词（如安全操作规则、Memory 过滤规则、子代理验证指引等）与终端 UI（如大文件告警、Doctor 诊断、外部导入确认弹窗等）中彻底清除 `CLAUDE.md`，统一展示为 `AGENTS.md`。
