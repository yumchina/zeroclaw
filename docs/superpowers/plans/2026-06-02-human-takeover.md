# Human-in-the-Loop Takeover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 当 ZeroClaw 智能体发生步骤超时、死循环或步骤失败时，向用户发送结构化的交互卡片（重试、干预、终止），使用户可以直接输入指令纠偏并继续任务，极大提升复杂任务执行的可视性与人机交互体验。

**Architecture:** 基于 WuKongIM 20号交互卡片（`WkMessageType::INTERACTIVE_CARD`）和 21号响应回执，在 Orchestrator 核心调度回路中拦截步骤超时与迭代错误，利用 `oneshot` 特征提供轻量级的会话流式挂起与拦截。

**Tech Stack:** Rust 2024, Tokio (oneshot & timeout), WuKongIM Custom Cards

---

### Task 1: API & Channel Trait Extensions

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`

- [x] **Step 1: Define Takeover Data Structures**
  Add the definitions of `ChannelInterventionRequest` and `ChannelInterventionResponse` representing intervention actions.

```rust
/// Request for human takeover/intervention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelInterventionRequest {
    pub reason: String,              // Halting reason (e.g. "Step Timeout", "Loop Detected")
    pub last_tool: Option<String>,    // Last executed tool (if any)
    pub error_detail: String,         // Specific error text/details
}

/// Operator response to intervention request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelInterventionResponse {
    Retry,
    Cancel,
    Intervene,
}
```

- [x] **Step 2: Add request_intervention to Channel Trait**
  Implement the default return signature in the `Channel` trait.

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    // ...
    async fn request_intervention(
        &self,
        _recipient: &str,
        _request: &ChannelInterventionRequest,
    ) -> anyhow::Result<Option<ChannelInterventionResponse>> {
        Ok(None)
    }
}
```

- [x] **Step 3: Verify Compilation**
  Run: `cargo check -p zeroclaw-api`
  Expected: Finished successfully.

---

### Task 2: WuKongIM Channel Implementation

**Files:**
- Create/Modify: `crates/zeroclaw-channel-wukongim/src/approval/card.rs`
- Create/Modify: `crates/zeroclaw-channel-wukongim/src/approval/mod.rs`
- Modify: `crates/zeroclaw-channel-wukongim/src/channel.rs`

- [x] **Step 1: Build Type-20 Intervention Card**
  Create card layout containing strictly `retry`, `intervene`, and `cancel` buttons (no `SwitchModel` button as requested).

```rust
pub fn build_intervention_card(
    approval_id: &str,
    request: &ChannelInterventionRequest,
    timeout_secs: u64,
) -> WkApprovalCard {
    let content = format!(
        "⚠️ **智能体任务执行异常**\n\n\
         **原因**: {}\n\
         **最近执行的工具**: {}\n\
         **详细错误**: {}\n\n\
         ---\n\
         请选择接管操作：",
        request.reason,
        request.last_tool.as_deref().unwrap_or("无"),
        request.error_detail
    );

    WkApprovalCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        approval_id: approval_id.to_string(),
        timeout_secs,
        title: "⚠️ 任务异常需人工接管".to_string(),
        body: WkApprovalBody { content },
        actions: Some(vec![
            WkAction {
                text: "重试当前步骤".to_string(),
                value: "retry".to_string(),
                style: "primary".to_string(),
            },
            WkAction {
                text: "人工输入指令".to_string(),
                value: "intervene".to_string(),
                style: "success".to_string(),
            },
            WkAction {
                text: "终止任务".to_string(),
                value: "cancel".to_string(),
                style: "danger".to_string(),
            },
        ]),
    }
}
```

- [x] **Step 2: Add Pending Interventions Mapping**
  Implement the oneshot maps in `WuKongIMChannel` for resolving clicks asynchronously.

```rust
pub type PendingInterventions = Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<ChannelInterventionResponse>>>>;
```

- [x] **Step 3: Resolve Clicks in process_inbound_message**
  Intercept inbound `21` response type actions and complete the oneshot channels:

```rust
if let Some(tx) = self.pending_interventions.lock().unwrap().remove(&action.approval_id) {
    let response = match action.action.as_str() {
        "retry" => ChannelInterventionResponse::Retry,
        "intervene" => ChannelInterventionResponse::Intervene,
        _ => ChannelInterventionResponse::Cancel,
    };
    let _ = tx.send(response);
}
```

- [x] **Step 4: Implement request_intervention method**
  Send the type-20 card to the client and wait for response with a dynamic timeout retrieved from configuration `self.approval_timeout_secs`:

```rust
async fn request_intervention(
    &self,
    recipient: &str,
    request: &ChannelInterventionRequest,
) -> anyhow::Result<Option<ChannelInterventionResponse>> {
    let approval_id = uuid::Uuid::new_v4().to_string();
    let card = build_intervention_card(&approval_id, request, self.approval_timeout_secs);
    // Send Card message ...
    // Wait for oneshot channel with timeout ...
}
```

- [x] **Step 5: Verify Compilation**
  Run: `cargo check -p zeroclaw-channel-wukongim`
  Expected: Finished successfully.

---

### Task 3: Orchestrator Integration & Suspended Message Interception

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [x] **Step 1: Add Global Suspended Tasks Registry**
  Create a standard-library thread-safe global lock to track hung tasks.

```rust
fn suspended_tasks() -> &'static StdMutex<HashMap<String, tokio::sync::oneshot::Sender<ChannelMessage>>> {
    static MAP: OnceLock<StdMutex<HashMap<String, tokio::sync::oneshot::Sender<ChannelMessage>>>> = OnceLock::new();
    MAP.get_or_init(|| StdMutex::new(HashMap::new()))
}
```

- [x] **Step 2: Intercept User Instructions in Message Dispatch Loop**
  Intercept incoming user manual instructions and dispatch them directly to the active suspended worker:

```rust
let scope_key = interruption_scope_key(&msg);
let suspended_sender = {
    let mut active = suspended_tasks().lock().unwrap();
    active.remove(&scope_key)
};
if let Some(tx) = suspended_sender {
    let _ = tx.send(msg);
    continue;
}
```

- [x] **Step 3: Hook into run_tool_call_loop execution outcomes**
  Intercept LLM results. If errors or step timeouts occur, invoke human takeover card:

```rust
let last_tool = get_last_tool_from_history(&history);
let req = ChannelInterventionRequest { reason, last_tool, error_detail };
match channel.request_intervention(&msg.reply_target, &req).await {
    Ok(Some(ChannelInterventionResponse::Retry)) => continue,
    Ok(Some(ChannelInterventionResponse::Intervene)) => {
        // Wait for oneshot input, push to history and continue loop
    }
    _ => {} // abort task
}
```

- [x] **Step 4: Verify Entire Test Suite**
  Run: `cargo test`
  Expected: All 164 unit and system integration tests PASS successfully.
