# 设计：进度上报 i18n 化 + 结构化（progress-observer i18n + structured payload）

> 编写日期：2026-06-05
> 状态：设计已确认，待编写实现计划
> 背景：源于对 `crates/zeroclaw-channels/src/orchestrator/progress.rs` 的两点改进诉求

## 1. 问题陈述

0.8.0 的进度上报（`progress.rs`）存在两个问题：

1. **写死中文**：`format_tool_start_desc` / `event_to_status` 把中文文案硬编码在代码里，
   违反项目 i18n 惯例。0.8.0 其他面向用户的文案都走 Fluent（`cli.ftl`/`tools.ftl`），
   唯独进度文本是例外。

2. **丢弃结构化信息**：`event_to_status` 返回一个 `String`，把 `ObserverEvent` 的其他属性
   （`tool_call_id`/`success`/`duration` 等）丢弃。下游 channel 只收到一句文本，前端无法做
   富渲染（例如按 `tool_call_id` 原地更新同一个工具气泡），只能「刷屏」式显示多条独立消息。

## 2. 现状（已核实）

- **i18n 框架**：Fluent（`.ftl`）。资源在 `crates/zeroclaw-runtime/locales/<locale>/{cli,tools}.ftl`，
  英文 `include_str!` 编译时嵌入作为 fallback，zh-CN 等内置 + 磁盘覆盖。加载基础设施
  （`FluentBundle`、磁盘覆盖、locale 检测、`locales.toml` 注册表）集中在 `runtime/src/i18n.rs`。
- **runtime `pub mod i18n` 已导出**，`channels` 已依赖 `runtime` → channels 可直接调
  `zeroclaw_runtime::i18n::*`。
- **数据源**：0.8.0 的 `ObserverEvent::ToolCallStart` / `ToolCall` 已携带
  `tool` / `tool_call_id` / `arguments` / `duration` / `success` / `result` 字段
  （定义于 `zeroclaw-api/src/observability_traits.rs`）。无需 master 那套 task-local
  （`CURRENT_TOOL_*` 在 0.8.0 已删除）。
- **Channel trait** 的 `update_draft_progress(recipient, message_id, text)` 是**默认 no-op**，
  仅 3 个真实实现者：DawnIM、matrix、slack。

## 3. 已确认的设计决策

| 决策点 | 选择 |
|--------|------|
| 翻译层级 | **runtime 端用 Fluent 翻译出本地化文本 + 附结构化字段**（前端零翻译负担） |
| i18n 资源位置 | **runtime crate 新增 `events.ftl`**（与 cli/tools 并列，复用现有基础设施） |
| 覆盖范围 | **全部 6 类事件**：AgentStart / AgentEnd / LlmRequest / ToolCallStart / ToolCall / Error |
| Channel 接口 | **方案 A：改 `update_draft_progress` 签名**，用 `ProgressUpdate` 替换 `text` |
| 数据源 | 复用 0.8.0 已有的 `ObserverEvent` 字段，**不引入 task-local** |

## 4. 架构与数据流

```
ObserverEvent（0.8.0 已有字段，无需 task-local）
  │  tool / tool_call_id / arguments / duration / success / ...
  ▼
ProgressObserver::record_event              [channels/orchestrator/progress.rs]
  │
  ▼ event_to_progress(event, cfg) -> Option<ProgressUpdate>
  │     ├─ text:  zeroclaw_runtime::i18n::get_event_string_with_args(key, args)   ← Fluent 本地化
  │     └─ phase: ProgressPhase::{...}                                            ← 从 event 字段填充
  ▼
channel.update_draft_progress(recipient, draft_id, &ProgressUpdate)   [Channel trait]
  ├─ DawnIM : text 兜底 + phase 结构化字段 → JSON payload（含 tool_call_id）
  ├─ matrix : 用 update.text（行为不变）
  └─ slack  : 用 update.text（行为不变）
```

核心原则：**数据源是已有的 `ObserverEvent`，翻译在 runtime 层，channel 收到「已本地化文本 + 结构化枚举」**。

## 5. 组件设计

### 5.1 `zeroclaw-api/src/channel.rs` — 新类型 + 签名变更

```rust
/// 已本地化的进度更新：text 兜底显示，phase 供富客户端渲染。
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub text: String,          // 生产者已 i18n 翻译好的兜底文本
    pub phase: ProgressPhase,  // 结构化事件数据
}

#[derive(Debug, Clone)]
pub enum ProgressPhase {
    AgentStart { provider: String, model: String },
    LlmRequest { messages_count: usize },
    ToolStart  { tool: String, tool_call_id: Option<String> },
    ToolDone   { tool: String, tool_call_id: Option<String>, success: bool, elapsed_ms: u64 },
    AgentEnd,
    Error      { component: String },
}

// 签名变更：text:&str → update:&ProgressUpdate（默认 no-op 保留）
async fn update_draft_progress(
    &self, _recipient: &str, _message_id: &str, _update: &ProgressUpdate,
) -> anyhow::Result<()> { Ok(()) }
```

设计借鉴 master 已验证的 `StatusUpdate`/`StatusPhase` 设计，但数据源换成 0.8.0 的 `ObserverEvent`。

### 5.2 `zeroclaw-runtime` i18n — 新增 events 类别

- **新增资源**：`crates/zeroclaw-runtime/locales/{en,zh-CN}/events.ftl`（与 `cli.ftl`/`tools.ftl`
  并列；其他 locale 缺失时按现有机制 fallback en）。
- **`i18n.rs` 泛化**：当前 `load_cli_strings` / `load_descriptions` 已高度重复，再加 events
  会三重复制。把加载逻辑抽成按 `(filename, builtin_source)` 参数的通用函数，然后新增对外入口：
  ```rust
  pub fn get_event_string(key: &str) -> Option<String>;
  pub fn get_event_string_with_args(key: &str, args: &[(&str, &str)]) -> String;
  ```
  这是「改动所在文件时顺带做的针对性改进」，不扩大到无关重构。
- **`events.ftl`（en 示例）**：
  ```
  event-agent-start         = Agent started ({$provider}/{$model})
  event-agent-end           = Processing complete
  event-llm-request         = Calling LLM ({$count} messages)
  event-tool-start-shell    = Running command: {$snippet}
  event-tool-start-generic  = Calling tool: {$tool}
  event-tool-done-success   = {$tool} completed ({$elapsed}ms)
  event-tool-done-failure   = {$tool} failed
  event-error               = {$component} error: {$message}
  ```
  `zh-CN/events.ftl` 即现在写死的中文文案（"执行命令：…"等），原样搬入。

### 5.3 `channels/orchestrator/progress.rs` — 翻译层重写

- `event_to_status(...) -> Option<String>` 改为 `event_to_progress(...) -> Option<ProgressUpdate>`。
- 工具名 → i18n key 的映射（替代写死的 `format_tool_start_desc`）：`shell`→`event-tool-start-shell`
  等，未匹配走 `event-tool-start-generic`。
- `summarize_tool_args`（提取 snippet）保留。
- `ProgressObserver::record_event` 改为构造 `ProgressUpdate` 并调新签名。

### 5.4 三个实现者改动

| Channel | 改动 |
|---------|------|
| **DawnIM** (`dawn_im/channel.rs`) | 签名改 `&ProgressUpdate`；payload 中 `text` 兜底 + 把 `phase` 的结构化字段（`tool_name`/`tool_call_id`/`success`/`elapsed_ms`）一并写入 JSON。这一步顺带补上了 master `45c3179a4` 的 tool_call_id 透传，为前端原地更新气泡打基础。 |
| **matrix** / **slack** | 签名改 `&ProgressUpdate`，显示内容用 `update.text`，行为不变 |

## 6. 错误处理

- 沿用现有 fire-and-forget：进度推送失败只记 DEBUG 日志，不阻塞 agent loop。
- i18n key 缺失走现有兜底机制（记 WARN + 返回 `{key}`）。
- locale 缺失 `events.ftl` 时 fallback en（现有机制）。

## 7. 测试策略

1. **i18n**：`events.ftl` 各 key 在 en/zh-CN 能正确 format（仿现有 `cli.ftl` 测试模式）。
2. **翻译层**：`event_to_progress` 对 6 类事件返回正确的 `text` + `phase`。
3. **DawnIM**：`update_draft_progress` 产出的 payload 含结构化字段（`tool_call_id` 等）。

## 8. 兼容性与影响面

- Channel trait 签名变更由**编译器强制**所有实现者更新，无遗漏风险。
- 默认 no-op 实现保留，未实现 progress 的 channel 不受影响。
- 影响文件：`zeroclaw-api/src/channel.rs`、`zeroclaw-runtime/src/i18n.rs`、
  `zeroclaw-runtime/locales/*/events.ftl`（新增）、`channels/orchestrator/progress.rs`、
  `channels/src/dawn_im/channel.rs`、`channels/src/matrix.rs`、`channels/src/slack.rs`。

## 9. 与 master 合并计划的关系

本设计是 [2026-06-05-master-to-080-merge-plan.md](../plans/2026-06-05-master-to-080-merge-plan.md)
「第二步」的替代方案。原计划是移植 master `45c3179a4`（task-local 路线），但调研发现 0.8.0
已用 ObserverEvent 字段化取代了 task-local，且进度可见功能已等价实现。本设计在 0.8.0 既有
架构上推进，顺带补齐 master 想要的 tool_call_id 透传，**不引入 task-local**，避免两套并行机制。
