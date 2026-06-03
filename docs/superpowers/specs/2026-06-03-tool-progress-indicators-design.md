# 设计方案：WuKongIM 通道工具进度标识支持

本文档详细说明了如何在 ZeroClaw 的通道进度状态更新中，增加程序化的工具启动和完成标识（包括工具名称、工具调用唯一 ID、状态以及执行耗时）的设计方案。

## 背景与动机
目前，ZeroClaw 的进度观察器（Progress Observer）通过 `send_status_update` 向通信通道报告工具执行的生命周期事件（开始和完成）。然而，发送的载荷（Payload）存在以下局限性：
1. `content.type` 字段被硬编码为 `"tool"`，导致前端无法在程序层面感知具体执行的是哪个工具（如 `"shell"`、`"file_read"`）。
2. 工具状态/阶段仅通过 `title` 字段中的中文本地化字符串表达（例如 `"工具启动"`、`"工具完成"`、`"工具失败"`），不利于前端进行稳健的程序化解析。
3. 状态更新中没有携带唯一的 `tool_call_id`。当大模型并行执行多个工具时，前端无法将“工具完成”事件与相应的“工具启动”事件进行精确匹配。

为了解决这些问题，我们将在进度观察器事件、状态更新结构体以及 WuKongIM JSON 载荷中引入结构化的程序化字段。

---

## 详细设计

### 1. 扩展 `ObserverEvent`
我们将在可观测性事件（Observability Traits）的 `ToolCallStart` 和 `ToolCall` 事件中新增 `tool_call_id` 属性，用以携带工具调用的唯一 ID：

```rust
// 位于 crates/zeroclaw-api/src/observability_traits.rs
pub enum ObserverEvent {
    // ...
    ToolCallStart {
        tool: String,
        arguments: Option<String>,
        tool_call_id: Option<String>, // 新增
    },
    ToolCall {
        tool: String,
        duration: Duration,
        success: bool,
        tool_call_id: Option<String>, // 新增
    },
    // ...
}
```

同时，我们还将同步更新整个代码库中引用/实现 `ObserverEvent` 的所有适配器（例如 OTel、Prometheus 以及 Gateway SSE 适配器等），以兼容这个新增的 Option 属性。

### 2. 更新工具执行逻辑以传递 `tool_call_id`
在运行时的工具执行模块中，我们将从 `ParsedToolCall` 中提取 `tool_call_id`，并将其传递给单工具执行器 `execute_one_tool`，以便在记录事件时将其包含进去：

```rust
// 位于 crates/zeroclaw-runtime/src/agent/tool_execution.rs
pub async fn execute_one_tool(
    call_name: &str,
    call_arguments: serde_json::Value,
    tool_call_id: Option<String>, // 新增
    // ...
)
```

### 3. 扩展 `StatusUpdate` 结构体
我们将在 `StatusUpdate` 中增加程序化字段，以便将工具遥测数据传递给通道适配器：

```rust
// 位于 crates/zeroclaw-api/src/channel.rs
pub struct StatusUpdate {
    pub execution_id: String,
    pub phase: StatusPhase,
    pub name: String,
    pub desc: String,
    pub tool_name: Option<String>,    // 新增：工具名称
    pub tool_call_id: Option<String>, // 新增：大模型工具调用唯一 ID
}
```

### 4. 增强 WuKongIM 通道状态更新载荷
在 WuKongIM 通道的 `send_status_update` 实现中，我们将 `StatusPhase` 映射为程序化的 `status` 字符串，并将这些新字段序列化到 JSON 载荷的 `content` 对象中：

```rust
// 位于 crates/zeroclaw-channel-wukongim/src/channel.rs
async fn send_status_update(
    &self,
    recipient: &str,
    _thread_ts: Option<&str>,
    update: StatusUpdate,
) -> anyhow::Result<()> {
    // ...
    let status_str = match &update.phase {
        StatusPhase::AgentStart => "agent_start",
        StatusPhase::LlmThinking => "thinking",
        StatusPhase::ToolStart => "tool_start",
        StatusPhase::ToolDone { success: true, .. } => "tool_success",
        StatusPhase::ToolDone { success: false, .. } => "tool_failed",
        StatusPhase::Error => "error",
        StatusPhase::AgentEnd => "agent_end",
    };

    let mut content = serde_json::json!({
        "title": phase_to_content(&update.phase),
        "mid": update.execution_id,
        "type": update.name,
        "desc": update.desc,
        "status": status_str,
    });

    // 如果可选字段存在，则合并至 content 中
    if let Some(tool_name) = update.tool_name {
        content["tool_name"] = serde_json::json!(tool_name);
    }
    if let Some(tool_call_id) = update.tool_call_id {
        content["tool_call_id"] = serde_json::json!(tool_call_id);
    }
    if let StatusPhase::ToolDone { success, elapsed_ms } = update.phase {
        content["success"] = serde_json::json!(success);
        content["elapsed_ms"] = serde_json::json!(elapsed_ms);
    }

    let payload = serde_json::json!({
        "type": 23,
        "content": content,
    });

    self.send_status_message(&channel_id, channel_type, payload).await
}
```

---

## 关于版本兼容性

### 1. 对旧前端的兼容性（100% 兼容）
* **原有字段完全保留**：`title`、`mid`、`type`、`desc` 四个字段的结构、键名和原始输出完全保持不变（例如 `type` 仍然保持 `"tool"`，`title` 仍然是中文字符串）。
* **新增字段为增量**：新增的 `"status"`、`"tool_name"`、`"tool_call_id"` 等均作为 `content` 对象的**增量属性**添加。不支持这些新字段的旧前端会直接忽略它们，继续像以前一样正常工作和渲染，不会发生崩溃或行为异常。

### 2. 代码向后兼容性
* `StatusUpdate` 上的新增字段为 `Option<String>`。如果没有传入，则在序列化为 JSON 时会自动忽略，其他不感知此变化的内部测试或通道适配器能直接正常编译运行。

---

## 验证计划

### 自动化测试
- 运行单元测试与集成测试：`cargo test`
- 运行静态检查：`cargo clippy --all-targets -- -D warnings`
- 运行格式化检查：`cargo fmt --all -- --check`

### 手动验证
- 检查 JSON 载荷的序列化结构，确保 `tool_name`、`tool_call_id`、`status`、`success` 和 `elapsed_ms` 的数据结构和值均符合预期。
