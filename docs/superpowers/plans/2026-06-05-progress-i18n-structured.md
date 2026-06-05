# 进度上报 i18n 化 + 结构化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `progress.rs` 的写死中文进度文本改为 Fluent i18n，并让进度更新携带结构化字段（含 tool_call_id）传到 channel。

**Architecture:** 数据源沿用 0.8.0 已有的 `ObserverEvent` 字段（无 task-local）。runtime 端用 Fluent 生成本地化文本 + 填充 `ProgressPhase` 结构化枚举，打包成 `ProgressUpdate` 经 `Channel::update_draft_progress` 传给 channel。DawnIM 富渲染（写结构化字段进 payload），matrix/slack 用兜底 text。

**Tech Stack:** Rust, Fluent (`.ftl`), tokio, serde_json。

参考设计：[../specs/2026-06-05-progress-i18n-structured-design.md](../specs/2026-06-05-progress-i18n-structured-design.md)

---

## 文件结构

| 文件 | 职责 | 动作 |
|------|------|------|
| `crates/zeroclaw-runtime/locales/en/events.ftl` | 英文事件文案（fallback） | 创建 |
| `crates/zeroclaw-runtime/locales/zh-CN/events.ftl` | 中文事件文案 | 创建 |
| `crates/zeroclaw-runtime/src/i18n.rs` | 新增 events 加载 + `get_event_string*`，去重加载逻辑 | 修改 |
| `crates/zeroclaw-api/src/channel.rs` | 新增 `ProgressUpdate`/`ProgressPhase`，改 `update_draft_progress` 签名 | 修改 |
| `crates/zeroclaw-channels/src/orchestrator/progress.rs` | `event_to_status`→`event_to_progress`，构造 `ProgressUpdate` | 修改 |
| `crates/zeroclaw-channels/src/dawn_im/messaging.rs` | 新增 `encode_progress_payload` | 修改 |
| `crates/zeroclaw-channels/src/dawn_im/channel.rs` | `update_draft_progress` 改签名 + 写结构化字段 | 修改 |
| `crates/zeroclaw-channels/src/matrix.rs` | `update_draft_progress` 改签名（用 `update.text`） | 修改 |
| `crates/zeroclaw-channels/src/slack.rs` | `update_draft_progress` 改签名（用 `update.text`） | 修改 |

---

## Task 1: i18n events 资源 + 加载入口

**Files:**
- Create: `crates/zeroclaw-runtime/locales/en/events.ftl`
- Create: `crates/zeroclaw-runtime/locales/zh-CN/events.ftl`
- Modify: `crates/zeroclaw-runtime/src/i18n.rs`
- Test: `crates/zeroclaw-runtime/src/i18n.rs`（内联 `#[cfg(test)]`）

- [ ] **Step 1: 创建英文资源 `crates/zeroclaw-runtime/locales/en/events.ftl`**

```ftl
# Progress event strings, consumed by the channel progress observer.
event-agent-start = Agent started ({ $provider }/{ $model })
event-agent-end = Done
event-llm-request = Calling LLM ({ $count } messages)
event-tool-start-shell = Running command: { $snippet }
event-tool-start-web-search = Searching: { $snippet }
event-tool-start-read-file = Reading file: { $snippet }
event-tool-start-http = HTTP request: { $snippet }
event-tool-start-generic = Calling tool: { $tool }
event-tool-done-success = { $tool } completed ({ $elapsed }ms)
event-tool-done-failure = { $tool } failed
event-error = { $component } error: { $message }
```

- [ ] **Step 2: 创建中文资源 `crates/zeroclaw-runtime/locales/zh-CN/events.ftl`**

```ftl
# 进度事件文案，供 channel 进度观察器使用。
event-agent-start = Agent 启动（{ $provider }/{ $model }）
event-agent-end = 处理完成
event-llm-request = 正在调用大模型推理（{ $count } 条消息）
event-tool-start-shell = 执行命令：{ $snippet }
event-tool-start-web-search = 搜索：{ $snippet }
event-tool-start-read-file = 读取文件：{ $snippet }
event-tool-start-http = HTTP 请求：{ $snippet }
event-tool-start-generic = 调用工具：{ $tool }
event-tool-done-success = { $tool } 执行完成（{ $elapsed }ms）
event-tool-done-failure = { $tool } 执行失败
event-error = { $component } 出现错误：{ $message }
```

- [ ] **Step 3: 写失败测试（i18n.rs 的 tests 模块末尾，`}` 之前）**

在 `crates/zeroclaw-runtime/src/i18n.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn events_ftl_formats_en_and_zh() {
        // English (embedded fallback)
        let en = include_str!("../locales/en/events.ftl");
        let shell = format_ftl_message(en, "en", "event-tool-start-shell", &[("snippet", "ls -la")])
            .expect("en event-tool-start-shell should format");
        assert_eq!(shell, "Running command: ls -la");
        let done = format_ftl_message(en, "en", "event-tool-done-success",
            &[("tool", "shell"), ("elapsed", "456")])
            .expect("en event-tool-done-success should format");
        assert_eq!(done, "shell completed (456ms)");

        // Chinese (builtin)
        let zh = include_str!("../locales/zh-CN/events.ftl");
        let shell_zh = format_ftl_message(zh, "zh-CN", "event-tool-start-shell", &[("snippet", "ls -la")])
            .expect("zh event-tool-start-shell should format");
        assert_eq!(shell_zh, "执行命令：ls -la");
        let agent_zh = format_ftl_message(zh, "zh-CN", "event-agent-start",
            &[("provider", "openai"), ("model", "gpt-5")])
            .expect("zh event-agent-start should format");
        assert_eq!(agent_zh, "Agent 启动（openai/gpt-5）");
    }

    #[test]
    fn get_event_string_with_args_falls_back_to_en() {
        // Unknown locale → English events.ftl fallback.
        let sources = load_event_ftl_sources("xx-FAKE");
        let value = format_string_with_args(
            &sources,
            include_str!("../locales/en/events.ftl"),
            "event-tool-done-failure",
            &[("tool", "shell")],
        )
        .expect("fallback to en should format");
        assert_eq!(value, "shell failed");
    }
```

- [ ] **Step 4: 运行测试确认失败**

Run: `cargo test -p zeroclaw-runtime events_ftl_formats_en_and_zh get_event_string_with_args_falls_back_to_en 2>&1 | tail -20`
Expected: 编译失败 —— `load_event_ftl_sources` / `format_string_with_args` 未定义。

- [ ] **Step 5: 泛化加载逻辑（去重）**

在 `i18n.rs` 中，把 `load_cli_strings` / `load_descriptions` 改为薄包装，新增通用 `load_strings` 与 events 加载。

替换现有 `load_descriptions`（约 132-150 行）与 `load_cli_strings`（约 159-170 行）为：

```rust
/// Generic FTL catalogue loader: English embedded fallback, then builtin
/// locale overrides, then disk overrides. Shared by tools / cli / events.
fn load_strings(
    locale: &str,
    en_ftl: &str,
    builtin: Option<&'static str>,
    filename: &str,
) -> HashMap<String, String> {
    let mut map = format_ftl_messages(en_ftl, "en");
    if locale != "en" {
        if let Some(b) = builtin {
            map.extend(format_ftl_messages(b, locale));
        }
        if let Some(disk) = load_ftl_from_disk(locale, filename) {
            map.extend(format_ftl_messages(&disk, locale));
        }
    }
    map
}

fn load_descriptions(locale: &str) -> HashMap<String, String> {
    load_strings(
        locale,
        include_str!("../locales/en/tools.ftl"),
        builtin_tools_ftl_source(locale),
        "tools.ftl",
    )
}

fn load_cli_strings(locale: &str) -> HashMap<String, String> {
    load_strings(
        locale,
        include_str!("../locales/en/cli.ftl"),
        builtin_cli_ftl_source(locale),
        "cli.ftl",
    )
}

fn load_event_strings(locale: &str) -> HashMap<String, String> {
    load_strings(
        locale,
        include_str!("../locales/en/events.ftl"),
        builtin_events_ftl_source(locale),
        "events.ftl",
    )
}

fn builtin_events_ftl_source(locale: &str) -> Option<&'static str> {
    match locale {
        "zh-CN" => Some(include_str!("../locales/zh-CN/events.ftl")),
        _ => None,
    }
}
```

- [ ] **Step 6: 泛化 args 格式化 + 新增 events 入口**

把现有 `format_cli_string_with_args`（约 191-207 行）改为调用泛化版，并新增 events 版本。在 `format_cli_string_with_args` 上方新增通用函数：

```rust
/// Generic argumented FTL resolution: disk → builtin → English fallback.
fn format_string_with_args(
    sources: &CliFtlSources,
    en_ftl: &str,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    if let Some(locale_ftl) = sources.disk.as_deref()
        && let Some(value) = format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(value);
    }
    if let Some(locale_ftl) = sources.builtin
        && let Some(value) = format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(value);
    }
    format_ftl_message(en_ftl, "en", key, args)
}
```

把现有 `format_cli_string_with_args` 函数体替换为：

```rust
fn format_cli_string_with_args(
    sources: &CliFtlSources,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    format_string_with_args(sources, include_str!("../locales/en/cli.ftl"), key, args)
}
```

新增 events 的 sources 加载器（放在 `load_cli_ftl_sources` 下方，约 182 行后）：

```rust
fn load_event_ftl_sources(locale: &str) -> CliFtlSources {
    CliFtlSources {
        locale: locale.to_string(),
        disk: (locale != "en")
            .then(|| load_ftl_from_disk(locale, "events.ftl"))
            .flatten(),
        builtin: (locale != "en")
            .then(|| builtin_events_ftl_source(locale))
            .flatten(),
    }
}
```

- [ ] **Step 7: 新增 events 缓存 + 公开入口 + init 接线**

在文件顶部 statics 区（约 10-13 行，`static LOCALE` 附近）新增：

```rust
static EVENT_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static EVENT_FTL_SOURCES: OnceLock<CliFtlSources> = OnceLock::new();
```

在 `init`（约 64-69 行）末尾追加两行：

```rust
    EVENT_STRINGS.get_or_init(|| load_event_strings(locale));
    EVENT_FTL_SOURCES.get_or_init(|| load_event_ftl_sources(locale));
```

在 `get_required_cli_string_with_args`（约 97 行）下方新增公开入口：

```rust
/// Get an event string by key (e.g. "event-agent-end").
pub fn get_event_string(key: &str) -> Option<String> {
    let map = EVENT_STRINGS.get_or_init(|| load_event_strings(active_locale()));
    map.get(key).cloned()
}

/// Get an event string by key, formatted with Fluent external arguments.
/// Falls back to the raw `{key}` marker (logged) when the key is missing.
pub fn get_event_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    let sources = EVENT_FTL_SOURCES.get_or_init(|| load_event_ftl_sources(active_locale()));
    format_string_with_args(
        sources,
        include_str!("../locales/en/events.ftl"),
        key,
        args,
    )
    .unwrap_or_else(|| missing_cli_string(key))
}
```

- [ ] **Step 8: 运行测试确认通过**

Run: `cargo test -p zeroclaw-runtime i18n 2>&1 | tail -20`
Expected: PASS（含新增两个测试 + 现有 i18n 测试不回归）。

- [ ] **Step 9: Commit**

```bash
git add crates/zeroclaw-runtime/locales/en/events.ftl crates/zeroclaw-runtime/locales/zh-CN/events.ftl crates/zeroclaw-runtime/src/i18n.rs
git commit -m "feat(i18n): add events.ftl catalogue and get_event_string entries"
```

---

## Task 2: 在 zeroclaw-api 定义 ProgressUpdate / ProgressPhase

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`
- Test: `crates/zeroclaw-api/src/channel.rs`（内联 `#[cfg(test)]`）

本任务只新增类型，不改 trait 签名（保持可独立编译）。

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-api/src/channel.rs` 末尾的 `#[cfg(test)] mod tests`（若不存在则新建）中追加：

```rust
#[cfg(test)]
mod progress_update_tests {
    use super::{ProgressPhase, ProgressUpdate};

    #[test]
    fn progress_update_holds_text_and_phase() {
        let u = ProgressUpdate {
            text: "shell completed (5ms)".to_string(),
            phase: ProgressPhase::ToolDone {
                tool: "shell".to_string(),
                tool_call_id: Some("call_1".to_string()),
                success: true,
                elapsed_ms: 5,
            },
        };
        assert_eq!(u.text, "shell completed (5ms)");
        match u.clone().phase {
            ProgressPhase::ToolDone { tool, success, elapsed_ms, tool_call_id } => {
                assert_eq!(tool, "shell");
                assert!(success);
                assert_eq!(elapsed_ms, 5);
                assert_eq!(tool_call_id.as_deref(), Some("call_1"));
            }
            _ => panic!("expected ToolDone phase"),
        }
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p zeroclaw-api progress_update 2>&1 | tail -20`
Expected: 编译失败 —— `ProgressUpdate` / `ProgressPhase` 未定义。

- [ ] **Step 3: 新增类型定义**

在 `crates/zeroclaw-api/src/channel.rs` 中，`ChannelInterventionResponse` 枚举定义之后（约第 48 行附近，即「Channel intervention/takeover types」区块下方）插入：

```rust
// ── Progress reporting types ─────────────────────────────────────

/// A localized + structured progress update for draft/status reporting.
/// `text` is pre-localized fallback text; `phase` carries structured data
/// for rich clients (e.g. updating a tool bubble in place by `tool_call_id`).
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub text: String,
    pub phase: ProgressPhase,
}

/// Structured progress phase data, mirrors the relevant `ObserverEvent`
/// variants the progress observer translates.
#[derive(Debug, Clone)]
pub enum ProgressPhase {
    AgentStart { provider: String, model: String },
    LlmRequest { messages_count: usize },
    ToolStart { tool: String, tool_call_id: Option<String> },
    ToolDone { tool: String, tool_call_id: Option<String>, success: bool, elapsed_ms: u64 },
    AgentEnd,
    Error { component: String },
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p zeroclaw-api progress_update 2>&1 | tail -20`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-api/src/channel.rs
git commit -m "feat(api): add ProgressUpdate/ProgressPhase types"
```

---

## Task 3: 切换进度链路到 ProgressUpdate（breaking change）

改 `Channel::update_draft_progress` 签名是 breaking change，必须一次性改完 trait + 3 个实现者 + progress.rs 调用点才能编译通过。本任务内的 step 是 bite-sized，但 commit 在全部改完、编译通过后进行。

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs:317`（trait 签名）
- Modify: `crates/zeroclaw-channels/src/orchestrator/progress.rs`
- Modify: `crates/zeroclaw-channels/src/dawn_im/messaging.rs`
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs:1129`
- Modify: `crates/zeroclaw-channels/src/matrix.rs:3526`
- Modify: `crates/zeroclaw-channels/src/slack.rs:4004`
- Test: `progress.rs`（内联）、`messaging.rs`（内联）

- [ ] **Step 1: 改 trait 签名**

在 `crates/zeroclaw-api/src/channel.rs` 找到（约 317-324 行）：

```rust
    async fn update_draft_progress(
        &self,
        _recipient: &str,
        _message_id: &str,
        _text: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }
```

替换 `_text: &str,` 一行为 `_update: &ProgressUpdate,`：

```rust
    async fn update_draft_progress(
        &self,
        _recipient: &str,
        _message_id: &str,
        _update: &ProgressUpdate,
    ) -> anyhow::Result<()> {
        Ok(())
    }
```

- [ ] **Step 2: 改写 progress.rs 的翻译层 — 写新失败测试**

在 `crates/zeroclaw-channels/src/orchestrator/progress.rs` 的 `mod tests` 中，删除旧的 `event_to_status_*` 系列测试（约 303-419 行的 8 个 `event_to_status_*` 函数），替换为以下基于 `event_to_progress` 的测试（断言 phase 结构 + text 含关键变量值，避免依赖全局 locale）：

```rust
    #[test]
    fn event_to_progress_tool_start_known_tool() {
        let cfg = all_on();
        let event = ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            tool_call_id: Some("call_9".into()),
            arguments: Some(r#"{"command": "ls -la"}"#.into()),
        };
        let u = event_to_progress(&event, &cfg).expect("should translate");
        assert!(u.text.contains("ls -la"), "text was: {}", u.text);
        match u.phase {
            ProgressPhase::ToolStart { tool, tool_call_id } => {
                assert_eq!(tool, "shell");
                assert_eq!(tool_call_id.as_deref(), Some("call_9"));
            }
            _ => panic!("expected ToolStart"),
        }
    }

    #[test]
    fn event_to_progress_tool_start_generic_for_unknown_tool() {
        let cfg = all_on();
        let event = ObserverEvent::ToolCallStart {
            tool: "weird_custom_tool".into(),
            tool_call_id: None,
            arguments: None,
        };
        let u = event_to_progress(&event, &cfg).expect("should translate");
        assert!(u.text.contains("weird_custom_tool"), "text was: {}", u.text);
        assert!(matches!(u.phase, ProgressPhase::ToolStart { .. }));
    }

    #[test]
    fn event_to_progress_tool_done_success_carries_fields() {
        let cfg = all_on();
        let event = ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: Some("call_1".into()),
            duration: Duration::from_millis(456),
            success: true,
            arguments: None,
            result: None,
        };
        let u = event_to_progress(&event, &cfg).expect("should translate");
        assert!(u.text.contains("456"), "text was: {}", u.text);
        match u.phase {
            ProgressPhase::ToolDone { tool, tool_call_id, success, elapsed_ms } => {
                assert_eq!(tool, "shell");
                assert_eq!(tool_call_id.as_deref(), Some("call_1"));
                assert!(success);
                assert_eq!(elapsed_ms, 456);
            }
            _ => panic!("expected ToolDone"),
        }
    }

    #[test]
    fn event_to_progress_agent_start_phase() {
        let cfg = all_on();
        let event = ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-5".into(),
        };
        let u = event_to_progress(&event, &cfg).expect("should translate");
        assert!(u.text.contains("openai"), "text was: {}", u.text);
        match u.phase {
            ProgressPhase::AgentStart { provider, model } => {
                assert_eq!(provider, "openai");
                assert_eq!(model, "gpt-5");
            }
            _ => panic!("expected AgentStart"),
        }
    }

    #[test]
    fn event_to_progress_returns_none_when_toggle_off() {
        let cfg = ProgressObserverConfig {
            enabled: true,
            agent_start: false,
            agent_end: false,
            tool_call_start: false,
            tool_call: false,
            llm_thinking: false,
            error: false,
        };
        let event = ObserverEvent::ToolCall {
            tool: "shell".into(),
            tool_call_id: None,
            duration: Duration::from_millis(1),
            success: true,
            arguments: None,
            result: None,
        };
        assert!(event_to_progress(&event, &cfg).is_none());
    }
```

> 注：保留 `summarize_tool_args` 与 `truncate_chars` 的现有测试不动。

- [ ] **Step 3: 改 progress.rs imports + 删除 `format_tool_start_desc` + 实现 `event_to_progress`**

在 `crates/zeroclaw-channels/src/orchestrator/progress.rs` 顶部 import 区（约 22-26 行）补充：

```rust
use zeroclaw_api::channel::{Channel, ProgressPhase, ProgressUpdate};
```
（替换原来的 `use zeroclaw_api::channel::Channel;` 一行。）

删除现有 `format_tool_start_desc` 函数（约 61-69 行）。

把现有 `event_to_status` 函数（约 74-110 行）整体替换为：

```rust
fn tool_start_key_and_args<'a>(
    tool: &'a str,
    snippet: Option<&'a str>,
) -> (&'static str, Vec<(&'a str, &'a str)>) {
    match (tool, snippet) {
        ("shell", Some(s)) => ("event-tool-start-shell", vec![("snippet", s)]),
        ("web_search", Some(s)) => ("event-tool-start-web-search", vec![("snippet", s)]),
        ("read_file", Some(s)) => ("event-tool-start-read-file", vec![("snippet", s)]),
        ("http", Some(s)) => ("event-tool-start-http", vec![("snippet", s)]),
        (other, _) => ("event-tool-start-generic", vec![("tool", other)]),
    }
}

/// Translate an `ObserverEvent` to a localized + structured `ProgressUpdate`
/// when the matching toggle is enabled. Returns `None` for events outside the
/// 6 supported classes or when the relevant toggle is off.
pub(crate) fn event_to_progress(
    event: &ObserverEvent,
    cfg: &ProgressObserverConfig,
) -> Option<ProgressUpdate> {
    use zeroclaw_runtime::i18n::get_event_string_with_args;
    match event {
        ObserverEvent::AgentStart {
            model_provider,
            model,
        } if cfg.agent_start => {
            let text = get_event_string_with_args(
                "event-agent-start",
                &[("provider", model_provider), ("model", model)],
            );
            Some(ProgressUpdate {
                text,
                phase: ProgressPhase::AgentStart {
                    provider: model_provider.clone(),
                    model: model.clone(),
                },
            })
        }
        ObserverEvent::AgentEnd { .. } if cfg.agent_end => {
            let text = get_event_string_with_args("event-agent-end", &[]);
            Some(ProgressUpdate {
                text,
                phase: ProgressPhase::AgentEnd,
            })
        }
        ObserverEvent::LlmRequest { messages_count, .. } if cfg.llm_thinking => {
            let count = messages_count.to_string();
            let text = get_event_string_with_args("event-llm-request", &[("count", &count)]);
            Some(ProgressUpdate {
                text,
                phase: ProgressPhase::LlmRequest {
                    messages_count: *messages_count,
                },
            })
        }
        ObserverEvent::ToolCallStart {
            tool,
            tool_call_id,
            arguments,
        } if cfg.tool_call_start => {
            let snippet = summarize_tool_args(arguments.as_deref());
            let (key, args) = tool_start_key_and_args(tool, snippet.as_deref());
            let text = get_event_string_with_args(key, &args);
            Some(ProgressUpdate {
                text,
                phase: ProgressPhase::ToolStart {
                    tool: tool.clone(),
                    tool_call_id: tool_call_id.clone(),
                },
            })
        }
        ObserverEvent::ToolCall {
            tool,
            tool_call_id,
            duration,
            success,
            ..
        } if cfg.tool_call => {
            let elapsed_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
            let text = if *success {
                let elapsed = elapsed_ms.to_string();
                get_event_string_with_args(
                    "event-tool-done-success",
                    &[("tool", tool), ("elapsed", &elapsed)],
                )
            } else {
                get_event_string_with_args("event-tool-done-failure", &[("tool", tool)])
            };
            Some(ProgressUpdate {
                text,
                phase: ProgressPhase::ToolDone {
                    tool: tool.clone(),
                    tool_call_id: tool_call_id.clone(),
                    success: *success,
                    elapsed_ms,
                },
            })
        }
        ObserverEvent::Error { component, message } if cfg.error => {
            let truncated = truncate_chars(message, ERROR_MESSAGE_MAX_CHARS);
            let text = get_event_string_with_args(
                "event-error",
                &[("component", component), ("message", &truncated)],
            );
            Some(ProgressUpdate {
                text,
                phase: ProgressPhase::Error {
                    component: component.clone(),
                },
            })
        }
        _ => None,
    }
}
```

- [ ] **Step 4: 改 progress.rs 的 `ProgressObserver::record_event` 调用点**

在 `record_event`（约 150-176 行）中，把：

```rust
        if let Some(text) = event_to_status(event, &self.cfg) {
            let channel = Arc::clone(&self.channel);
            let recipient = self.recipient.clone();
            let draft_id = self.draft_message_id.clone().unwrap_or_default();
            tokio::spawn(async move {
                if let Err(e) = channel
                    .update_draft_progress(&recipient, &draft_id, &text)
                    .await
```

改为：

```rust
        if let Some(update) = event_to_progress(event, &self.cfg) {
            let channel = Arc::clone(&self.channel);
            let recipient = self.recipient.clone();
            let draft_id = self.draft_message_id.clone().unwrap_or_default();
            tokio::spawn(async move {
                if let Err(e) = channel
                    .update_draft_progress(&recipient, &draft_id, &update)
                    .await
```

（其余行不变。）

- [ ] **Step 5: 新增 DawnIM `encode_progress_payload`（messaging.rs）+ 测试**

在 `crates/zeroclaw-channels/src/dawn_im/messaging.rs` 顶部 import 区补充：

```rust
use zeroclaw_api::channel::ProgressPhase;
```

在 `encode_text_payload`（约 23-33 行）下方新增：

```rust
/// Encode a progress update payload: markdown `text` for fallback display,
/// plus structured fields from `phase` so rich clients can render in place
/// (e.g. update a tool bubble by `tool_call_id`).
pub fn encode_progress_payload(content: &str, phase: &ProgressPhase) -> anyhow::Result<String> {
    let mut inner = serde_json::json!({ "type": "markdown", "text": content });
    if let Some(obj) = inner.as_object_mut() {
        match phase {
            ProgressPhase::AgentStart { provider, model } => {
                obj.insert("phase".into(), serde_json::json!("agent_start"));
                obj.insert("provider".into(), serde_json::json!(provider));
                obj.insert("model".into(), serde_json::json!(model));
            }
            ProgressPhase::LlmRequest { messages_count } => {
                obj.insert("phase".into(), serde_json::json!("llm_request"));
                obj.insert("messages_count".into(), serde_json::json!(messages_count));
            }
            ProgressPhase::ToolStart { tool, tool_call_id } => {
                obj.insert("phase".into(), serde_json::json!("tool_start"));
                obj.insert("tool_name".into(), serde_json::json!(tool));
                if let Some(id) = tool_call_id {
                    obj.insert("tool_call_id".into(), serde_json::json!(id));
                }
            }
            ProgressPhase::ToolDone { tool, tool_call_id, success, elapsed_ms } => {
                obj.insert("phase".into(), serde_json::json!("tool_done"));
                obj.insert("tool_name".into(), serde_json::json!(tool));
                if let Some(id) = tool_call_id {
                    obj.insert("tool_call_id".into(), serde_json::json!(id));
                }
                obj.insert("success".into(), serde_json::json!(success));
                obj.insert("elapsed_ms".into(), serde_json::json!(elapsed_ms));
            }
            ProgressPhase::AgentEnd => {
                obj.insert("phase".into(), serde_json::json!("agent_end"));
            }
            ProgressPhase::Error { component } => {
                obj.insert("phase".into(), serde_json::json!("error"));
                obj.insert("component".into(), serde_json::json!(component));
            }
        }
    }
    let payload = serde_json::json!({ "type": 14, "content": inner });
    let json = serde_json::to_string(&payload)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(json))
}
```

在 `messaging.rs` 的 `#[cfg(test)] mod tests` 中追加（参考现有 `encode_text_payload_is_valid_base64_json`）：

```rust
    #[test]
    fn encode_progress_payload_includes_tool_call_id_and_fields() {
        use base64::Engine;
        use zeroclaw_api::channel::ProgressPhase;
        let b64 = encode_progress_payload(
            "💭 shell completed (5ms)",
            &ProgressPhase::ToolDone {
                tool: "shell".into(),
                tool_call_id: Some("call_42".into()),
                success: true,
                elapsed_ms: 5,
            },
        )
        .expect("encode should succeed");
        let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        assert_eq!(v["type"], 14);
        assert_eq!(v["content"]["tool_name"], "shell");
        assert_eq!(v["content"]["tool_call_id"], "call_42");
        assert_eq!(v["content"]["success"], true);
        assert_eq!(v["content"]["elapsed_ms"], 5);
        assert_eq!(v["content"]["phase"], "tool_done");
    }
```

- [ ] **Step 6: 改 DawnIM `update_draft_progress`（channel.rs）**

在 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 顶部，找到 `use zeroclaw_api::channel::{...}` 块，补充 `ProgressUpdate`（与现有导入的 `Channel`、`SendMessage` 等并列）。

将现有方法（约 1129-1161 行）替换为：

```rust
    async fn update_draft_progress(
        &self,
        recipient: &str,
        _message_id: &str,
        update: &zeroclaw_api::channel::ProgressUpdate,
    ) -> anyhow::Result<()> {
        let trimmed = update.text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let content = format!("💭 {trimmed}");
        let payload_b64 = crate::dawn_im::messaging::encode_progress_payload(&content, &update.phase)?;
        let (channel_id, channel_type) = parse_recipient(recipient);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: serde_json::Value::String(payload_b64),
            header: Some(Header {
                no_persist: Some(true),
                red_dot: Some(false),
                ..Default::default()
            }),
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }
```

> 注：若 `encode_progress_payload` 的模块路径与上面不同（取决于 `messaging` 在 `dawn_im/mod.rs` 的 `pub`/`pub(crate)` 声明），用 `messaging.rs` 实际暴露的路径。现有 `encode_text_payload` 的调用方式可作参照。

- [ ] **Step 7: 改 matrix.rs `update_draft_progress`**

在 `crates/zeroclaw-channels/src/matrix.rs`（约 3526-3538 行）替换为：

```rust
    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        update: &zeroclaw_api::channel::ProgressUpdate,
    ) -> Result<()> {
        // Tool-status updates only show in Partial (edit-in-place) mode.
        // MultiMessage doesn't have an in-flight draft to update.
        if matches!(self.config.stream_mode, StreamMode::Partial) {
            return self.update_draft(recipient, message_id, &update.text).await;
        }
        Ok(())
    }
```

- [ ] **Step 8: 改 slack.rs `update_draft_progress`**

在 `crates/zeroclaw-channels/src/slack.rs`（约 4004-4018 行）替换为：

```rust
    async fn update_draft_progress(
        &self,
        recipient: &str,
        _message_id: &str,
        update: &zeroclaw_api::channel::ProgressUpdate,
    ) -> anyhow::Result<()> {
        let status_line = update.text.trim().lines().last().unwrap_or("").trim();
        // Skip "Thinking..." — the typing indicator already conveys that.
        // Only show tool-related progress in the status bar.
        if status_line.is_empty() || status_line.starts_with("\u{1f914}") {
            return Ok(());
        }
        self.set_assistant_status(recipient, status_line).await;
        Ok(())
    }
```

- [ ] **Step 9: 全量编译**

Run: `cargo build -p zeroclaw-channels 2>&1 | tail -20`
Expected: 编译通过（无 `update_draft_progress` 签名不匹配错误）。

> 若有其他未列出的 `update_draft_progress` 实现者或调用点报错，编译器会指出位置；按相同模式（`text` → `update.text` / `&update`）修复。

- [ ] **Step 10: 运行测试**

Run: `cargo test -p zeroclaw-channels progress 2>&1 | tail -30`
Run: `cargo test -p zeroclaw-channels encode_progress 2>&1 | tail -20`
Expected: PASS（新 `event_to_progress_*` 测试 + `encode_progress_payload_*` 测试 + 保留的 `summarize_*` 测试）。

- [ ] **Step 11: Commit**

```bash
git add crates/zeroclaw-api/src/channel.rs \
  crates/zeroclaw-channels/src/orchestrator/progress.rs \
  crates/zeroclaw-channels/src/dawn_im/messaging.rs \
  crates/zeroclaw-channels/src/dawn_im/channel.rs \
  crates/zeroclaw-channels/src/matrix.rs \
  crates/zeroclaw-channels/src/slack.rs
git commit -m "feat(channels): localized + structured progress updates via ProgressUpdate"
```

---

## Task 4: 全量验证

- [ ] **Step 1: 工作区全量构建**

Run: `cargo build 2>&1 | tail -20`
Expected: 整个 workspace 编译通过。

- [ ] **Step 2: 相关 crate 测试**

Run: `cargo test -p zeroclaw-runtime -p zeroclaw-api -p zeroclaw-channels 2>&1 | tail -30`
Expected: 全部 PASS。

- [ ] **Step 3: clippy（与项目惯例一致时）**

Run: `cargo clippy -p zeroclaw-channels -p zeroclaw-runtime 2>&1 | tail -20`
Expected: 无新增 warning/error。

---

## Self-Review 记录

- **Spec 覆盖**：i18n 化（Task 1）✓；6 类事件全覆盖（Task 3 `event_to_progress`）✓；ProgressUpdate/ProgressPhase 结构化（Task 2）✓；方案 A 改签名（Task 3 Step 1）✓；DawnIM 透传 tool_call_id（Task 3 Step 5/6）✓；matrix/slack 用兜底 text（Task 3 Step 7/8）✓；i18n 资源放 runtime crate ✓。
- **Placeholder 扫描**：无 TBD/TODO；所有 step 含完整代码或确切命令。
- **类型一致性**：`ProgressUpdate{text, phase}`、`ProgressPhase::{AgentStart{provider,model}, LlmRequest{messages_count}, ToolStart{tool,tool_call_id}, ToolDone{tool,tool_call_id,success,elapsed_ms}, AgentEnd, Error{component}}` 在 Task 2 定义、Task 3 各处引用一致；`get_event_string_with_args`、`event_to_progress`、`encode_progress_payload` 在定义与调用点签名一致；event 字段名用 `model_provider`（与 `ObserverEvent` 实际定义一致），映射到 phase 的 `provider` 字段。
