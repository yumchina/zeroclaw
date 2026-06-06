# 设计：异常场景卡片（DawnIM 错误卡片化）

> 编写日期：2026-06-06
> 状态：设计已确认，待编写实现计划
> 背景：Human Takeover（master `e816fb804` / yumchina PR #34）移植的**第一阶段**

## 1. 问题陈述

0.8.0 当前在智能体任务非正常结束时，给 DawnIM 用户的反馈很差：

- 失败路径（死循环/超时/执行错误）只发**英文硬编码文本**（如 `⚠️ Error: Agent loop aborted by loop detector: …`），暴露技术细节、无本地化。
- 只有"上下文超限"被本地化（`ERR:context_window_exceeded` → DawnIM `send()` 拦截成中文文本）。
- 用户取消（/stop）只回 `"Stop signal sent."`；被新消息打断时无任何反馈。

目标：把这些非正常结束统一为**结构化、本地化的异常卡片**，明确告诉用户「发生了什么类型的问题」+「友好的说明」。

## 2. 范围

**本期只做「纯展示卡片」**（无交互按钮）。完整 Human Takeover 的交互能力（Retry / Intervene / Cancel 按钮 + 指令注入）留待二期——本期的卡片结构为其预留扩展位（`actions` 字段）。

## 3. 现状（已核实）

- **失败判定**：orchestrator 通过 `LlmExecutionResult` 枚举的返回值区分结局：`Completed(Ok(Ok))`=成功、`Completed(Ok(Err))`=内部错误（含死循环 `bail`）、`Completed(Err(Elapsed))`=超时、`Cancelled`=取消。不依赖事件（0.8.0 这些路径不发 `ObserverEvent`，只写 `zeroclaw_log`）。
- **失败反馈落点**：`orchestrator/mod.rs` 的 `4613`（错误，含死循环/上下文/cancel 子分支）、`4766`（超时）。
- **取消/打断落点**：`/stop` 在 `4975`（当前发 "Stop signal sent." 文本，能访问 channel + reply_target）；被新消息打断在 `4870`（当前只记日志）。两处是分开的代码位置，天然可区分原因。
- **DawnIM `send()`**：已有 `ERR:context_window_exceeded` 的拦截本地化模式（结构化码 → channel 渲染）。DawnIM 不支持 draft 流式（错误都走 `send()`）。
- **dawn_im 审批卡片基础设施**：`approval.rs` 已有 `WkApprovalCard`/`WkAction`/`build_approval_card`、`INTERACTIVE_CARD` 类型、卡片按钮回调、`request_approval`——可参照其卡片构建与发送模式。
- **i18n**：Fluent，资源在 runtime crate；已有泛化的 `load_strings` 加载器和 `get_event_string`（前序 progress 工作建立），可对称新增 `errors` 类。

## 4. 已确认的设计决策

| 决策点 | 选择 |
|--------|------|
| 卡片交互 | 纯展示，无按钮（单向发送） |
| 卡片内容 | 两段式：错误类型标题（reason）+ 友好详情（detail） |
| 文案 | 全 i18n（Fluent），标题与详情均为按类型预设的友好中文；技术细节只进 `zeroclaw_log` |
| 触发范围 | 全部非正常结束：死循环/超时/执行错误/上下文超限/取消/被打断（共 6 种） |
| 送达架构 | 方案 B：orchestrator 发 `ERR:` 结构化码，DawnIM `send()` 拦截渲染卡片 |
| 卡片结构 | 新建独立 `WkExceptionCard`（不复用 `WkApprovalCard`），预留 `actions` 便于二期扩展 |
| 分级 | 卡片带 `level`（error / cancelled），区分 heading 与前端样式 |

## 5. 架构与数据流

```
非正常结束 → orchestrator 分类成 ERR 码 → channel.send(SendMessage::new("ERR:xxx"))
                                                  │
                                                  ▼
                              DawnIM send() 拦截 "ERR:" 前缀
                                  │  build_exception_card(code) → 查 i18n
                                  ▼
                          发 INTERACTIVE_CARD 卡片 payload（actions=None）
                                  │
                                  ▼  其他 channel：不拦截 → 显示码原文（降级，by design）
```

技术错误细节（`e.to_string()`）**仍照常进 `zeroclaw_log`** 供运维诊断，**不进卡片**。

## 6. 职责划分（关键）

| 类别 | 触发点 | 谁发卡片 | 当前行为 |
|------|--------|---------|---------|
| 任务失败（死循环/超时/执行错误/上下文超限） | 4613 / 4766 | 在结果处理点发 | 发英文文本 / context_window 已发码 |
| 用户取消（/stop） | 4975 | 在 /stop 点发 | 发 "Stop signal sent." 文本 |
| 被新消息打断 | 4870 | 在 interrupt 点发 | 仅记日志 |

被取消的 agent loop 走到 `4613` cancel 子分支时**不发卡片**（避免与取消发起点重复）；其 `cancel_draft` 在 DawnIM 本就是 no-op。

## 7. 六个 ERR 码 + level

| ERR 码 | level | 触发条件 |
|--------|-------|---------|
| `ERR:loop_detected` | error | 4613，`e` 含 `loop detector`/`circuit breaker` |
| `ERR:step_timeout` | error | 4766（`Completed(Err(Elapsed))`） |
| `ERR:step_error` | error | 4613 兜底（其余错误） |
| `ERR:context_window_exceeded` | error | 4613 上下文溢出（已有码，升级为卡片） |
| `ERR:cancelled` | cancelled | 4975（主动 /stop） |
| `ERR:interrupted` | cancelled | 4870（被新消息打断） |

分类 helper（orchestrator）：把 `anyhow::Error` / 超时映射为码字符串。死循环靠 `e.to_string()` 含 `loop detector`/`circuit breaker` 判定。

## 8. 组件设计

### 8.1 独立卡片结构（新 `dawn_im/exception_card.rs`）

```rust
#[derive(Debug, Serialize, Deserialize)]
pub struct WkExceptionCard {
    pub msg_type: u8,                    // INTERACTIVE_CARD
    pub kind: String,                    // 错误码，如 "step_timeout"
    pub level: String,                   // "error" | "cancelled"
    pub heading: String,                 // i18n，按 level
    pub reason: String,                  // i18n，按 kind
    pub detail: String,                  // i18n，按 kind
    pub actions: Option<Vec<WkAction>>,  // 预留，本期恒为 None
}

pub fn build_exception_card(code: &str) -> WkExceptionCard;
```

`build_exception_card` 内部：根据 code 决定 `level`，查 `get_error_string` 填 `heading`/`reason`/`detail`，未知 code 兜底为 `step_error`。`WkAction` 复用 approval.rs 既有定义（仅类型复用，不复用 WkApprovalCard）。

### 8.2 DawnIM `send()` 拦截升级（`dawn_im/channel.rs`）

```rust
async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
    if let Some(code) = message.content.strip_prefix("ERR:") {
        let card = build_exception_card(code);
        let payload_b64 = base64(serde_json::to_string(&card)?);
        // SendParams（INTERACTIVE_CARD payload）→ 现有 WebSocket 发送路径
    } else {
        // 现有普通文本路径（encode_text_payload）
    }
}
```

### 8.3 i18n（runtime crate `errors.ftl`）

复用 `load_strings` 泛化基础设施，新增 `errors` 类 + `get_error_string`/`get_error_string_with_args`（仿 `get_event_string`）。

`errors.ftl`（en，zh-CN 对应全角中文）：
```
error-heading-error = ⚠️ Agent task error
error-heading-cancelled = ℹ️ Task ended
error-loop-detected-reason = Loop Detected
error-loop-detected-detail = The agent got stuck in a loop and was stopped. Please retry or rephrase.
error-step-timeout-reason = Step Timeout
error-step-timeout-detail = The model response timed out. Please try again.
error-step-error-reason = Step Error
error-step-error-detail = The task failed to execute. Please retry or contact an admin.
error-context-window-exceeded-reason = Context Window Exceeded
error-context-window-exceeded-detail = The conversation is too long for the model. Please trim and retry.
error-cancelled-reason = Cancelled
error-cancelled-detail = You stopped the current task.
error-interrupted-reason = Interrupted
error-interrupted-detail = The previous task was interrupted by your new message.
```
zh-CN 用全角标点（：（）），与前序 events.ftl 一致。

### 8.4 卡片外观

```
level=error:                          level=cancelled:
⚠️ 智能体任务异常                      ℹ️ 任务已结束
──────────────────                    ──────────────────
异常原因: 步骤超时 (Step Timeout)      原因: 已取消 (Cancelled)
错误详情: 模型响应超时，请稍后重试      详情: 您已停止当前任务。
```
说明：`heading` 的本地化文案**已内置 emoji**（⚠️ / ℹ️），后端提供 `heading`/`reason`/`detail` 三个自包含、可直接显示的本地化文案值 + `level`/`kind`；字段标签（"异常原因:"/"原因:"）与配色由前端按 `level` 可选渲染（不依赖后端，也不假设前端必有此能力——即使前端只平铺三个文案值也可读）。

## 9. 兼容性

- 未知 ERR 码 → 兜底 `step_error`。
- 非 DawnIM channel 的 `send()` 不拦截 `ERR:` → 显示码原文（与现有 context_window 行为一致，by design，提示运维为该 channel 添加拦截）。
- `WkExceptionCard` 独立于 `WkApprovalCard`，互不影响现有审批流程。

## 10. 测试策略

1. **i18n**：`errors.ftl` 6 个 code 的 reason/detail 在 en/zh-CN 能正确 format；两个 heading 能 format。
2. **卡片渲染**：DawnIM `send()` 对 6 种 `ERR:` 码产出的 payload 解码后，含正确的 `kind`/`level`/`heading`/`reason`/`detail` 且 `msg_type` 为 INTERACTIVE_CARD、`actions` 为 None。
3. **错误分类**：orchestrator 的 错误→码 分类 helper 单测（死循环/执行错误/超时 映射正确）。

## 11. 涉及文件

- `crates/zeroclaw-channels/src/orchestrator/mod.rs`：4613/4766/4870/4975 改为发对应 ERR 码（4613 cancel 子分支不发卡片）。
- `crates/zeroclaw-channels/src/dawn_im/channel.rs`：`send()` 加 `ERR:` 拦截渲染卡片。
- `crates/zeroclaw-channels/src/dawn_im/exception_card.rs`：新建，`WkExceptionCard` + `build_exception_card`。
- `crates/zeroclaw-runtime/locales/{en,zh-CN}/errors.ftl`：新建。
- `crates/zeroclaw-runtime/src/i18n.rs`：加 `errors` 类 + `get_error_string`。

## 12. 与 Human Takeover / 合并计划的关系

本设计是 [2026-06-05-master-to-080-merge-plan.md](../plans/2026-06-05-master-to-080-merge-plan.md) 「第 3 步 Human Takeover（`e816fb804`）」的**第一阶段**。

- **本期（错误卡片化）**：把非正常结束统一为本地化展示卡片，解决"英文技术错误直接糊脸"的体验问题。不碰 orchestrator 交互逻辑、不需要 `suspended_tasks`、不改消息分发入口——低风险。
- **二期（完整交互接管）**：在 `WkExceptionCard.actions` 上加 Retry/Intervene/Cancel 按钮，引入 `request_intervention` 请求-响应、`suspended_tasks` 指令注入、`run_message_dispatch_loop` 入口拦截——对齐 master 的完整 Human Takeover。届时需注意与 0.8.0 现有 model switch（`/model`）逻辑并存。
