# 异常场景卡片（DawnIM 错误卡片化）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把智能体任务的非正常结束（死循环/超时/执行错误/上下文超限/取消/被打断）统一为本地化的展示型异常卡片，通过 DawnIM 发给用户。

**Architecture:** orchestrator 在失败/取消/打断处发 `ERR:<code>` 结构化码（沿用 0.8.0 既有的 context_window 模式），DawnIM `send()` 拦截 `ERR:` 前缀 → `build_exception_card` 查 Fluent i18n 渲染成 `WkExceptionCard`（INTERACTIVE_CARD，无按钮）→ 发卡片。技术细节仍只进 `zeroclaw_log`。

**Tech Stack:** Rust, Fluent (`.ftl`), serde_json, base64, tokio。

参考设计：[../specs/2026-06-06-error-card-design.md](../specs/2026-06-06-error-card-design.md)

⚠️ **关键提醒**：`dawn_im` 模块是 `channel-dawnIM` feature-gated，默认 `cargo test`/`build` 不编译它。Task 2/3 的构建与测试必须带 `--features channel-dawnIM`。

---

## 文件结构

| 文件 | 职责 | 动作 |
|------|------|------|
| `crates/zeroclaw-runtime/locales/en/errors.ftl` | 英文错误文案 | 创建 |
| `crates/zeroclaw-runtime/locales/zh-CN/errors.ftl` | 中文错误文案（全角标点） | 创建 |
| `crates/zeroclaw-runtime/src/i18n.rs` | 加 `errors` 类加载 + `get_error_string` | 修改 |
| `crates/zeroclaw-channels/src/dawn_im/exception_card.rs` | `WkExceptionCard` + `build_exception_card` | 创建 |
| `crates/zeroclaw-channels/src/dawn_im/mod.rs` | 声明 `exception_card` 模块 | 修改 |
| `crates/zeroclaw-channels/src/dawn_im/channel.rs` | `send()` 加 `ERR:` 拦截渲染卡片 | 修改 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | 4613/4766/4870/4975 发 ERR 码 + 分类 helper | 修改 |

任务依赖：Task 1 → Task 2 → Task 3；Task 4 独立于 2/3（只发字符串码）；Task 5 验证。

---

## Task 1: i18n errors 资源 + 加载入口

**Files:**
- Create: `crates/zeroclaw-runtime/locales/en/errors.ftl`
- Create: `crates/zeroclaw-runtime/locales/zh-CN/errors.ftl`
- Modify: `crates/zeroclaw-runtime/src/i18n.rs`
- Test: `crates/zeroclaw-runtime/src/i18n.rs`（内联 `#[cfg(test)]`）

- [ ] **Step 1: 创建 `crates/zeroclaw-runtime/locales/en/errors.ftl`**

```ftl
# Exception card strings, consumed by the DawnIM error card renderer.
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

- [ ] **Step 2: 创建 `crates/zeroclaw-runtime/locales/zh-CN/errors.ftl`**（全角标点 ：（））

```ftl
# 异常卡片文案，供 DawnIM 错误卡片渲染器使用。
error-heading-error = ⚠️ 智能体任务异常
error-heading-cancelled = ℹ️ 任务已结束
error-loop-detected-reason = 死循环检测 (Loop Detected)
error-loop-detected-detail = 检测到智能体陷入循环，已自动停止。请稍后重试或换一种说法。
error-step-timeout-reason = 步骤超时 (Step Timeout)
error-step-timeout-detail = 模型响应超时，请稍后重试。
error-step-error-reason = 步骤执行错误 (Step Error)
error-step-error-detail = 任务执行出错，请稍后重试或联系管理员。
error-context-window-exceeded-reason = 上下文超限 (Context Window Exceeded)
error-context-window-exceeded-detail = 对话内容过长超出模型上限，请精简后重试。
error-cancelled-reason = 已取消 (Cancelled)
error-cancelled-detail = 您已停止当前任务。
error-interrupted-reason = 已中断 (Interrupted)
error-interrupted-detail = 上一个任务已被您的新消息中断。
```

- [ ] **Step 3: 写失败测试**（`i18n.rs` 的 `mod tests` 末尾追加）

```rust
    #[test]
    fn errors_ftl_formats_en_and_zh() {
        let en = include_str!("../locales/en/errors.ftl");
        assert_eq!(
            format_ftl_message(en, "en", "error-step-timeout-reason", &[]).as_deref(),
            Some("Step Timeout")
        );
        let zh = include_str!("../locales/zh-CN/errors.ftl");
        assert_eq!(
            format_ftl_message(zh, "zh-CN", "error-step-timeout-reason", &[]).as_deref(),
            Some("步骤超时 (Step Timeout)")
        );
        assert_eq!(
            format_ftl_message(zh, "zh-CN", "error-heading-error", &[]).as_deref(),
            Some("⚠️ 智能体任务异常")
        );
        assert_eq!(
            format_ftl_message(zh, "zh-CN", "error-cancelled-detail", &[]).as_deref(),
            Some("您已停止当前任务。")
        );
    }

    #[test]
    fn get_error_string_loads_from_catalogue() {
        // load_error_strings builds the catalogue; English fallback for unknown locale.
        let map = load_error_strings("xx-FAKE");
        assert_eq!(
            map.get("error-step-error-reason").map(String::as_str),
            Some("Step Error")
        );
    }
```

- [ ] **Step 4: 运行确认失败**

Run: `cargo test -p zeroclaw-runtime errors_ftl_formats_en_and_zh get_error_string_loads_from_catalogue 2>&1 | tail -20`
Expected: 编译失败（`load_error_strings` 未定义）。

- [ ] **Step 5: 加 errors 加载函数**（`i18n.rs`，与 `load_event_strings` 对称）

在 `builtin_events_ftl_source` 函数下方新增：

```rust
fn load_error_strings(locale: &str) -> HashMap<String, String> {
    load_strings(
        locale,
        include_str!("../locales/en/errors.ftl"),
        builtin_errors_ftl_source(locale),
        "errors.ftl",
    )
}

fn builtin_errors_ftl_source(locale: &str) -> Option<&'static str> {
    match locale {
        "zh-CN" => Some(include_str!("../locales/zh-CN/errors.ftl")),
        _ => None,
    }
}
```

- [ ] **Step 6: 加 errors 缓存 static + init 接线 + 公开入口**

在顶部 statics 区（`EVENT_STRINGS` / `EVENT_FTL_SOURCES` 附近）新增：

```rust
static ERROR_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
```

在 `init` 末尾（`EVENT_STRINGS.get_or_init(...)` 之后）追加：

```rust
    ERROR_STRINGS.get_or_init(|| load_error_strings(locale));
```

在 `get_event_string` 函数下方新增公开入口：

```rust
/// Get an error-card string by key (e.g. "error-step-timeout-reason").
pub fn get_error_string(key: &str) -> Option<String> {
    let map = ERROR_STRINGS.get_or_init(|| load_error_strings(active_locale()));
    map.get(key).cloned()
}
```

- [ ] **Step 7: 运行测试确认通过**

Run: `cargo test -p zeroclaw-runtime errors_ftl get_error_string 2>&1 | tail -20`
Expected: PASS（含两个新测试，且现有 i18n 测试不回归）。

- [ ] **Step 8: Commit**

```bash
git add crates/zeroclaw-runtime/locales/en/errors.ftl crates/zeroclaw-runtime/locales/zh-CN/errors.ftl crates/zeroclaw-runtime/src/i18n.rs
git commit -m "feat(i18n): add errors.ftl catalogue and get_error_string entry"
```

---

## Task 2: WkExceptionCard + build_exception_card

**Files:**
- Create: `crates/zeroclaw-channels/src/dawn_im/exception_card.rs`
- Modify: `crates/zeroclaw-channels/src/dawn_im/mod.rs`
- Test: `crates/zeroclaw-channels/src/dawn_im/exception_card.rs`（内联）

依赖 Task 1 的 `get_error_string`。dawn_im 是 `channel-dawnIM` feature-gated。

- [ ] **Step 1: 在 `dawn_im/mod.rs` 声明模块**

`mod.rs` 现有 `pub mod approval;` 等。在 `pub mod connection;` 之后加一行：

```rust
pub mod exception_card;
```

- [ ] **Step 2: 创建 `dawn_im/exception_card.rs`（含失败测试）**

```rust
//! Exception scene card for DawnIM.
//!
//! Renders non-normal task endings (loop/timeout/error/context/cancel/interrupt)
//! as a localized display card. Distinct from `approval.rs` (tool-call approval
//! flow); reuses only `WkAction` for the reserved `actions` field.

use serde::{Deserialize, Serialize};

use super::approval::WkAction;
use super::connection::WkMessageType;

/// Display-only exception card. `actions` is reserved for a future interactive
/// Human Takeover phase and is `None` in this phase.
#[derive(Debug, Serialize, Deserialize)]
pub struct WkExceptionCard {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub kind: String,
    pub level: String,
    pub heading: String,
    pub reason: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<WkAction>>,
}

/// Known error codes (without the `ERR:` prefix). Unknown codes fall back to
/// `step_error`.
const KNOWN_CODES: &[&str] = &[
    "loop_detected",
    "step_timeout",
    "step_error",
    "context_window_exceeded",
    "cancelled",
    "interrupted",
];

/// Build an exception card for an `ERR:` code (prefix already stripped).
/// Looks up localized heading/reason/detail via the runtime i18n catalogue.
pub fn build_exception_card(code: &str) -> WkExceptionCard {
    let code = if KNOWN_CODES.contains(&code) {
        code
    } else {
        "step_error"
    };
    let level = match code {
        "cancelled" | "interrupted" => "cancelled",
        _ => "error",
    };
    let key_code = code.replace('_', "-");
    let heading = zeroclaw_runtime::i18n::get_error_string(&format!("error-heading-{level}"))
        .unwrap_or_default();
    let reason = zeroclaw_runtime::i18n::get_error_string(&format!("error-{key_code}-reason"))
        .unwrap_or_default();
    let detail = zeroclaw_runtime::i18n::get_error_string(&format!("error-{key_code}-detail"))
        .unwrap_or_default();
    WkExceptionCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        kind: code.to_string(),
        level: level.to_string(),
        heading,
        reason,
        detail,
        actions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_card_sets_kind_level_and_type() {
        let card = build_exception_card("step_timeout");
        assert_eq!(card.msg_type, WkMessageType::INTERACTIVE_CARD);
        assert_eq!(card.kind, "step_timeout");
        assert_eq!(card.level, "error");
        assert!(card.actions.is_none());
        // i18n value present (en default in test env)
        assert!(!card.reason.is_empty(), "reason should be populated");
        assert!(!card.detail.is_empty(), "detail should be populated");
        assert!(!card.heading.is_empty(), "heading should be populated");
    }

    #[test]
    fn cancelled_and_interrupted_are_cancelled_level() {
        assert_eq!(build_exception_card("cancelled").level, "cancelled");
        assert_eq!(build_exception_card("interrupted").level, "cancelled");
    }

    #[test]
    fn unknown_code_falls_back_to_step_error() {
        let card = build_exception_card("totally_unknown");
        assert_eq!(card.kind, "step_error");
        assert_eq!(card.level, "error");
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM exception_card 2>&1 | tail -20`
Expected: 失败（先编译失败再到测试，确认模块接入）。修正任何编译错误直至下一步通过。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM exception_card 2>&1 | tail -20`
Expected: PASS（3 个测试）。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-channels/src/dawn_im/exception_card.rs crates/zeroclaw-channels/src/dawn_im/mod.rs
git commit -m "feat(dawn_im): add WkExceptionCard + build_exception_card"
```

---

## Task 3: DawnIM send() 拦截渲染卡片

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`（`send()`，约 1056-1075）
- Test: `crates/zeroclaw-channels/src/dawn_im/channel.rs`（内联）或 `exception_card.rs`

依赖 Task 2。dawn_im feature-gated。

- [ ] **Step 1: 读现状**

READ `crates/zeroclaw-channels/src/dawn_im/channel.rs` 的 `send()`（约 1056）。当前开头是：
```rust
async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
    let content = match message.content.as_str() {
        "ERR:context_window_exceeded" => "⚠️ 模型服务暂时遇到问题，请稍后重试。",
        other => other,
    };
    let payload_b64 = encode_text_payload(content)?;
    ...
```
确认顶部 `use super::approval::{...}` 一带的导入，准备加入 `exception_card`。

- [ ] **Step 2: 写失败测试**（在 `exception_card.rs` 的 `mod tests` 追加，验证卡片序列化 payload 结构）

```rust
    #[test]
    fn card_serializes_with_type_and_fields() {
        let card = build_exception_card("cancelled");
        let v = serde_json::to_value(&card).expect("serialize");
        assert_eq!(v["type"], 20); // INTERACTIVE_CARD
        assert_eq!(v["kind"], "cancelled");
        assert_eq!(v["level"], "cancelled");
        assert!(v.get("heading").is_some());
        assert!(v.get("reason").is_some());
        assert!(v.get("detail").is_some());
        // actions=None must be omitted (skip_serializing_if)
        assert!(v.get("actions").is_none());
    }
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM card_serializes_with_type_and_fields 2>&1 | tail -15`
Expected: 若 `actions` 未正确 skip 则失败；否则该测试用于锁定序列化契约。先运行确认其作为回归基线。

- [ ] **Step 4: 修改 `send()` 加 ERR 拦截**

在 `channel.rs` 顶部导入区加：
```rust
use super::exception_card::build_exception_card;
```
将 `send()` 开头的 content match + `encode_text_payload` 段替换为：

```rust
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let payload_b64 = if let Some(code) = message.content.strip_prefix("ERR:") {
            let card = build_exception_card(code);
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(&card)?)
        } else {
            encode_text_payload(&message.content)?
        };
```
（其余 `SendParams` 构造与 WebSocket 发送逻辑保持不变；删除原来的 `let content = match ... ;` 块和 `encode_text_payload(content)?` 行，由上面的 if/else 取代。注意原代码可能把 `content` 用于后续——确认只用于 `encode_text_payload`，若别处引用 `content` 需一并调整为 `&message.content`。）

- [ ] **Step 5: 运行测试 + 编译**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM exception_card 2>&1 | tail -20`
Run: `cargo build -p zeroclaw-channels --features channel-dawnIM 2>&1 | tail -8`
Expected: 测试 PASS，dawn feature 编译通过。

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-channels/src/dawn_im/channel.rs crates/zeroclaw-channels/src/dawn_im/exception_card.rs
git commit -m "feat(dawn_im): render ERR: codes as exception cards in send()"
```

---

## Task 4: orchestrator 发 ERR 码

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（4613 / 4766 / 4870 / 4975 + 新增分类 helper）
- Test: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（内联，分类 helper 单测）

orchestrator 默认编译（不依赖 dawn feature）；发的是字符串码。

- [ ] **Step 1: 写失败测试**（`mod.rs` 的 `mod tests` 追加，测分类 helper）

```rust
    #[test]
    fn classify_failure_code_maps_loop_and_error() {
        let loop_err = anyhow::anyhow!("Agent loop aborted by loop detector: ping-pong");
        assert_eq!(classify_failure_code(&loop_err), "ERR:loop_detected");
        let cb = anyhow::anyhow!("circuit breaker tripped");
        assert_eq!(classify_failure_code(&cb), "ERR:loop_detected");
        let other = anyhow::anyhow!("some provider 500");
        assert_eq!(classify_failure_code(&other), "ERR:step_error");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p zeroclaw-channels classify_failure_code 2>&1 | tail -15`
Expected: 编译失败（`classify_failure_code` 未定义）。

- [ ] **Step 3: 加分类 helper**

在 `mod.rs` 中 `process_channel_message` 之外的合适位置（靠近其他 free functions，如 `get_last_tool_from_history` 不存在于 0.8.0，则放近 `is_stop_command` 附近）新增：

```rust
/// Map an agent-loop error to a structured `ERR:` code for the failure card.
/// Loop-detector aborts (circuit breaker) → loop_detected; everything else →
/// step_error. (Context-window overflow is classified by its own branch before
/// this is reached.)
fn classify_failure_code(e: &anyhow::Error) -> &'static str {
    let s = e.to_string();
    if s.contains("loop detector") || s.contains("circuit breaker") {
        "ERR:loop_detected"
    } else {
        "ERR:step_error"
    }
}
```

- [ ] **Step 4: 4766 超时分支改发码**

READ 约 4798-4813。当前：
```rust
            if let Some(channel) = target_channel.as_ref() {
                let error_text =
                    "⚠️ Request timed out while waiting for the model. Please try again.";
```
把 `error_text` 改为：
```rust
                let error_text = "ERR:step_timeout";
```
（其余 `finalize_draft`/`send` 调用不变；DawnIM 走 `send()` 拦截成卡片。）

- [ ] **Step 5: 4613 普通错误 else 分支改发码**

READ 约 4750-4763（`else` 分支末尾发消息处）。当前：
```rust
                if let Some(channel) = target_channel.as_ref() {
                    if let Some(ref draft_id) = draft_message_id {
                        let _ = channel
                            .finalize_draft(&msg.reply_target, draft_id, &format!("⚠️ Error: {e}"))
                            .await;
                    } else {
                        let _ = channel
                            .send(
                                &SendMessage::new(format!("⚠️ Error: {e}"), &msg.reply_target)
                                    .in_thread(msg.thread_ts.clone()),
                            )
                            .await;
                    }
                }
```
把两处 `format!("⚠️ Error: {e}")` 改为 `classify_failure_code(&e)`：
```rust
                if let Some(channel) = target_channel.as_ref() {
                    let err_code = classify_failure_code(&e);
                    if let Some(ref draft_id) = draft_message_id {
                        let _ = channel
                            .finalize_draft(&msg.reply_target, draft_id, err_code)
                            .await;
                    } else {
                        let _ = channel
                            .send(
                                &SendMessage::new(err_code, &msg.reply_target)
                                    .in_thread(msg.thread_ts.clone()),
                            )
                            .await;
                    }
                }
```
（`e` 已在该分支作用域内；技术细节仍在前面的 `zeroclaw_log` WARN 中记录，不丢失。context_window 子分支与 cancel 子分支保持不变——前者已发 `ERR:context_window_exceeded`，后者不发卡片。）

- [ ] **Step 6: 4975 /stop 点改发码**

READ 约 4968-4998。当前：
```rust
            let reply = if let Some(state) = previous {
                state.cancellation.cancel();
                "Stop signal sent.".to_string()
            } else {
                "No in-flight task for this sender scope.".to_string()
            };
```
改为（仅在确有任务被取消时发卡片码）：
```rust
            let reply = if let Some(state) = previous {
                state.cancellation.cancel();
                "ERR:cancelled".to_string()
            } else {
                "No in-flight task for this sender scope.".to_string()
            };
```
（下方已有的 `channel.send(SendMessage::new(reply, ...))` 不变；DawnIM 把 `ERR:cancelled` 渲染成 cancelled 级卡片。"No in-flight task" 文本保持普通文本。）

- [ ] **Step 7: 4870 interrupt 点新增发码**

READ 约 4863-4872。当前：
```rust
        if interrupt_enabled && let Some(previous) = previous {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"sender": msg.sender})),
                "interrupting previous in-flight request for sender"
            );
            previous.cancellation.cancel();
            previous.completion.wait().await;
        }
```
在 `previous.completion.wait().await;` 之后新增：把 `ERR:interrupted` 卡片发到当前会话（同一 reply_target）。需要 channel 引用，复用 `/stop` 点的 `find_channel_for_message` 模式：
```rust
            previous.completion.wait().await;
            if let Some(channel) = find_channel_for_message(&ctx.channels_by_name, &msg).cloned() {
                let reply_target = msg.reply_target.clone();
                let thread_ts = msg.thread_ts.clone();
                zeroclaw_spawn::spawn!(async move {
                    let _ = channel
                        .send(
                            &SendMessage::new("ERR:interrupted", &reply_target)
                                .in_thread(thread_ts),
                        )
                        .await;
                });
            }
```
（READ 4863 附近确认 `ctx` 在 `dispatch_worker` 作用域内可用，且 `find_channel_for_message` 签名与 /stop 点一致：`find_channel_for_message(&ctx.channels_by_name, &msg)`。若 `ctx` 字段名不同，按实际调整。）

- [ ] **Step 8: 运行测试 + 编译**

Run: `cargo test -p zeroclaw-channels classify_failure_code 2>&1 | tail -15`
Run: `cargo build -p zeroclaw-channels 2>&1 | tail -8`
Expected: 测试 PASS，默认 feature 编译通过。

- [ ] **Step 9: Commit**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(orchestrator): emit ERR: codes for failures, cancel, interrupt"
```

---

## Task 5: 全量验证

- [ ] **Step 1: 默认 feature 全量构建**

Run: `cargo build 2>&1 | tail -8`
Expected: workspace 编译通过。

- [ ] **Step 2: dawn feature 构建 + 测试**

Run: `cargo build -p zeroclaw-channels --features channel-dawnIM 2>&1 | tail -8`
Run: `cargo test -p zeroclaw-channels --features channel-dawnIM exception_card 2>&1 | tail -20`
Expected: 编译通过，卡片测试全 PASS。

- [ ] **Step 3: 相关 crate 测试**

Run: `cargo test -p zeroclaw-runtime errors 2>&1 | tail -10`
Run: `cargo test -p zeroclaw-channels classify_failure_code 2>&1 | tail -10`
Expected: PASS。（注意：runtime 有 ~18 个、channels 有 2 个 pre-existing 失败，与本工作无关——分别属 media/shell/cron/attachments 与 prompt-date 测试。）

- [ ] **Step 4: clippy（改动 crate）**

Run: `cargo clippy -p zeroclaw-channels -p zeroclaw-runtime --features channel-dawnIM 2>&1 | tail -8`
Expected: 对改动文件无新 warning（disallowed `tokio::spawn` 等——本计划在 interrupt 点用的是 `zeroclaw_spawn::spawn!`，与项目惯例一致）。

---

## Self-Review 记录

- **Spec 覆盖**：6 个 ERR 码（Task 4 分类 + 4 个落点）✓；纯展示卡片无按钮（Task 2 `actions: None`）✓；两段式 reason+detail（Task 2/i18n）✓;全 i18n（Task 1 errors.ftl，zh 全角）✓;level 区分 error/cancelled（Task 2）✓;独立 WkExceptionCard 不复用 WkApprovalCard（Task 2）✓;方案 B ERR 码 + send 拦截（Task 3）✓;职责划分 失败在 4613/4766、取消/打断在 4975/4870（Task 4）✓;technical detail 进日志不进卡片（Task 4 保留 zeroclaw_log）✓;未知码兜底 step_error（Task 2）✓;非 DawnIM 降级（send 不拦截，原样文本）✓。
- **Placeholder 扫描**：无 TBD/TODO；每个改动步骤含完整代码或确切命令。
- **类型一致性**：`WkExceptionCard{msg_type:u32,kind,level,heading,reason,detail,actions:Option<Vec<WkAction>>}` 在 Task 2 定义、Task 3 序列化测试引用一致;`build_exception_card(code:&str)`、`classify_failure_code(&anyhow::Error)->&'static str`、`get_error_string(&str)->Option<String>` 跨任务签名一致;ERR 码字符串（`ERR:loop_detected`/`step_timeout`/`step_error`/`context_window_exceeded`/`cancelled`/`interrupted`）与 i18n key（`error-<code-with-dashes>-reason/-detail`）映射通过 `code.replace('_',"-")` 一致;`INTERACTIVE_CARD=20`。
- **依赖顺序**：Task 2 用 Task 1 的 `get_error_string`;Task 3 用 Task 2 的 `build_exception_card`;Task 4 独立。按 1→2→3→4→5 执行。
