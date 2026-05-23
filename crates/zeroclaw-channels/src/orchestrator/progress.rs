//! Per-message progress reporting via the existing draft-streaming path.
//!
//! Wires `ObserverEvent`s emitted by the agent loop into human-readable
//! Chinese status text that gets pushed through the orchestrator's existing
//! `StreamDelta::Status` channel — which the draft-updater task already
//! forwards to `Channel::update_draft_progress`. So we get
//! channel-specific status UX (Slack `assistant_status`, etc.) for free.
//!
//! Translation logic mirrors the master-fork `progress-observer` crate but
//! is intentionally stripped to text (no `StatusUpdate` / `StatusPhase`
//! structures) since the downstream path is text-only.
//!
//! Per-message context (which `recipient`, which `draft_message_id`) is
//! NOT in the `ObserverEvent` — the wrapper is constructed fresh for each
//! channel message and the mpsc sender it holds is already bound to the
//! correct delta channel, so no routing is needed.

use std::sync::Arc;

use tokio::sync::mpsc;
use zeroclaw_config::schema::ProgressObserverConfig;
use zeroclaw_runtime::agent::loop_::DraftEvent;
use zeroclaw_runtime::observability::{Observer, ObserverEvent, traits::ObserverMetric};

const ARG_SNIPPET_MAX_CHARS: usize = 120;
const ERROR_MESSAGE_MAX_CHARS: usize = 200;

/// Extract a short user-facing snippet from a JSON tool-arguments blob.
///
/// Picks the first present key in `command` / `query` / `path` / `url` and
/// returns its string value truncated to `ARG_SNIPPET_MAX_CHARS` chars
/// (UTF-8 safe). Falls back to the raw JSON when no known key is present
/// or the input isn't valid JSON.
pub(crate) fn summarize_tool_args(args_json: Option<&str>) -> Option<String> {
    let raw = args_json?;
    if raw.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        for key in ["command", "query", "path", "url"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                return Some(truncate_chars(s, ARG_SNIPPET_MAX_CHARS));
            }
        }
    }
    Some(truncate_chars(raw, ARG_SNIPPET_MAX_CHARS))
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

fn format_tool_start_desc(tool: &str, snippet: Option<&str>) -> String {
    match (tool, snippet) {
        ("shell", Some(s)) => format!("执行命令：{s}"),
        ("web_search", Some(s)) => format!("搜索：{s}"),
        ("read_file", Some(s)) => format!("读取文件：{s}"),
        ("http", Some(s)) => format!("HTTP 请求：{s}"),
        (other, _) => format!("调用工具：{other}"),
    }
}

/// Translate an `ObserverEvent` to a Chinese status string when the
/// matching toggle is enabled. Returns `None` for events outside the
/// 6 supported classes or when the relevant toggle is off.
pub(crate) fn event_to_status(
    event: &ObserverEvent,
    cfg: &ProgressObserverConfig,
) -> Option<String> {
    match event {
        ObserverEvent::AgentStart {
            model_provider,
            model,
        } if cfg.agent_start => Some(format!("Agent 启动（{model_provider}/{model}）")),
        ObserverEvent::AgentEnd { .. } if cfg.agent_end => Some("处理完成".into()),
        ObserverEvent::LlmRequest { messages_count, .. } if cfg.llm_thinking => {
            Some(format!("正在调用大模型推理（{messages_count} 条消息）"))
        }
        ObserverEvent::ToolCallStart { tool, arguments } if cfg.tool_call_start => {
            let snippet = summarize_tool_args(arguments.as_deref());
            Some(format_tool_start_desc(tool, snippet.as_deref()))
        }
        ObserverEvent::ToolCall {
            tool,
            duration,
            success,
        } if cfg.tool_call => {
            let elapsed_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
            Some(if *success {
                format!("{tool} 执行完成（{elapsed_ms}ms）")
            } else {
                format!("{tool} 执行失败")
            })
        }
        ObserverEvent::Error { component, message } if cfg.error => Some(format!(
            "{component} 出现错误：{}",
            truncate_chars(message, ERROR_MESSAGE_MAX_CHARS)
        )),
        _ => None,
    }
}

/// Per-message wrapper observer.
///
/// Lifecycle is bound to a single `process_channel_message` call. The mpsc
/// sender it holds is the orchestrator's per-message `delta_tx`; the
/// existing draft-updater task on the other side routes
/// `StreamDelta::Status(text)` to `Channel::update_draft_progress`. So this
/// wrapper does no I/O of its own — it just translates and forwards.
///
/// When `sender` is `None` (channel doesn't support draft streaming) the
/// observer becomes a transparent pass-through.
pub(crate) struct ProgressObserver {
    inner: Arc<dyn Observer>,
    sender: Option<mpsc::Sender<DraftEvent>>,
    cfg: ProgressObserverConfig,
}

impl ProgressObserver {
    pub(crate) fn new(
        inner: Arc<dyn Observer>,
        sender: Option<mpsc::Sender<DraftEvent>>,
        cfg: ProgressObserverConfig,
    ) -> Self {
        Self { inner, sender, cfg }
    }
}

impl Observer for ProgressObserver {
    fn record_event(&self, event: &ObserverEvent) {
        if let (Some(sender), Some(text)) =
            (self.sender.as_ref(), event_to_status(event, &self.cfg))
        {
            // Non-blocking: drop the status update if the delta channel is
            // full / closed. The progress text is purely advisory — losing
            // one update is preferable to slowing down the agent loop.
            let _ = sender.try_send(DraftEvent::Status(text));
        }
        self.inner.record_event(event);
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        self.inner.record_metric(metric);
    }

    fn flush(&self) {
        self.inner.flush();
    }

    fn name(&self) -> &str {
        "progress"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn all_on() -> ProgressObserverConfig {
        ProgressObserverConfig {
            enabled: true,
            agent_start: true,
            agent_end: true,
            tool_call_start: true,
            tool_call: true,
            llm_thinking: true,
            error: true,
        }
    }

    // -----------------------------------------------------------------
    // summarize_tool_args
    // -----------------------------------------------------------------

    #[test]
    fn summarize_none_input_returns_none() {
        assert!(summarize_tool_args(None).is_none());
    }

    #[test]
    fn summarize_empty_input_returns_none() {
        assert!(summarize_tool_args(Some("")).is_none());
    }

    #[test]
    fn summarize_extracts_command_key() {
        let arg = r#"{"command": "grep -c TODO README.md"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("grep -c TODO README.md")
        );
    }

    #[test]
    fn summarize_extracts_query_key() {
        let arg = r#"{"query": "rust async runtime"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("rust async runtime")
        );
    }

    #[test]
    fn summarize_extracts_path_key() {
        let arg = r#"{"path": "./README.md"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("./README.md")
        );
    }

    #[test]
    fn summarize_extracts_url_key() {
        let arg = r#"{"url": "https://example.com/x"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn summarize_prefers_command_over_others_when_multiple_keys_present() {
        let arg = r#"{"command": "ls", "query": "ignored"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some("ls"));
    }

    #[test]
    fn summarize_falls_back_to_truncated_json_when_no_known_key() {
        let arg = r#"{"random": "x"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some(arg));
    }

    #[test]
    fn summarize_falls_back_to_truncated_raw_when_not_valid_json() {
        let arg = "garbage not-json";
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some(arg));
    }

    #[test]
    fn summarize_truncates_long_command_with_ellipsis() {
        let long_cmd = "x".repeat(200);
        let arg = format!(r#"{{"command":"{long_cmd}"}}"#);
        let out = summarize_tool_args(Some(&arg)).unwrap();
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), ARG_SNIPPET_MAX_CHARS + 1);
    }

    #[test]
    fn summarize_truncate_handles_multibyte_utf8_safely() {
        let s = "中".repeat(200);
        let out = truncate_chars(&s, 10);
        assert_eq!(out.chars().count(), 11);
        assert!(out.ends_with('…'));
    }

    // -----------------------------------------------------------------
    // event_to_status — per-class behaviour
    // -----------------------------------------------------------------

    #[test]
    fn event_to_status_agent_start_when_toggle_on() {
        let cfg = all_on();
        let event = ObserverEvent::AgentStart {
            model_provider: "openai".into(),
            model: "gpt-5".into(),
        };
        assert_eq!(
            event_to_status(&event, &cfg).as_deref(),
            Some("Agent 启动（openai/gpt-5）")
        );
    }

    #[test]
    fn event_to_status_agent_end_when_toggle_on() {
        let cfg = all_on();
        let event = ObserverEvent::AgentEnd {
            model_provider: "openai".into(),
            model: "gpt-5".into(),
            duration: Duration::from_millis(1234),
            tokens_used: None,
            cost_usd: None,
        };
        assert_eq!(event_to_status(&event, &cfg).as_deref(), Some("处理完成"));
    }

    #[test]
    fn event_to_status_llm_request_when_toggle_on() {
        let cfg = all_on();
        let event = ObserverEvent::LlmRequest {
            model_provider: "openai".into(),
            model: "gpt-5".into(),
            messages_count: 7,
        };
        assert_eq!(
            event_to_status(&event, &cfg).as_deref(),
            Some("正在调用大模型推理（7 条消息）")
        );
    }

    #[test]
    fn event_to_status_tool_call_start_uses_known_tool_template() {
        let cfg = all_on();
        let event = ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(r#"{"command": "ls -la"}"#.into()),
        };
        assert_eq!(
            event_to_status(&event, &cfg).as_deref(),
            Some("执行命令：ls -la")
        );
    }

    #[test]
    fn event_to_status_tool_call_start_falls_back_for_unknown_tool() {
        let cfg = all_on();
        let event = ObserverEvent::ToolCallStart {
            tool: "weird_custom_tool".into(),
            arguments: None,
        };
        assert_eq!(
            event_to_status(&event, &cfg).as_deref(),
            Some("调用工具：weird_custom_tool")
        );
    }

    #[test]
    fn event_to_status_tool_call_success_includes_elapsed() {
        let cfg = all_on();
        let event = ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(456),
            success: true,
        };
        assert_eq!(
            event_to_status(&event, &cfg).as_deref(),
            Some("shell 执行完成（456ms）")
        );
    }

    #[test]
    fn event_to_status_tool_call_failure_omits_elapsed() {
        let cfg = all_on();
        let event = ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(456),
            success: false,
        };
        assert_eq!(
            event_to_status(&event, &cfg).as_deref(),
            Some("shell 执行失败")
        );
    }

    #[test]
    fn event_to_status_error_truncates_message() {
        let cfg = all_on();
        let message = "x".repeat(300);
        let event = ObserverEvent::Error {
            component: "model_provider".into(),
            message: message.clone(),
        };
        let out = event_to_status(&event, &cfg).expect("should produce status");
        assert!(out.starts_with("model_provider 出现错误："));
        assert!(
            out.ends_with('…'),
            "long message must be truncated with ellipsis"
        );
    }

    // -----------------------------------------------------------------
    // toggle gating
    // -----------------------------------------------------------------

    #[test]
    fn event_to_status_returns_none_when_toggle_off_even_if_enabled() {
        let cfg = ProgressObserverConfig {
            enabled: true,
            ..Default::default()
        };
        let event = ObserverEvent::AgentEnd {
            model_provider: "openai".into(),
            model: "gpt-5".into(),
            duration: Duration::ZERO,
            tokens_used: None,
            cost_usd: None,
        };
        assert!(event_to_status(&event, &cfg).is_none());
    }

    #[test]
    fn event_to_status_ignores_events_outside_six_classes() {
        let cfg = all_on();
        let event = ObserverEvent::HeartbeatTick;
        assert!(event_to_status(&event, &cfg).is_none());

        let event = ObserverEvent::ChannelMessage {
            channel: "slack".into(),
            direction: "inbound".into(),
        };
        assert!(event_to_status(&event, &cfg).is_none());
    }

    #[test]
    fn config_any_enabled_requires_master_switch_and_one_sub_toggle() {
        let cfg = ProgressObserverConfig {
            enabled: false,
            agent_start: true,
            ..Default::default()
        };
        assert!(
            !cfg.any_enabled(),
            "any_enabled must be false when master switch off"
        );

        let cfg = ProgressObserverConfig {
            enabled: true,
            ..Default::default()
        };
        assert!(
            !cfg.any_enabled(),
            "any_enabled must be false when no sub-toggle on"
        );

        let cfg = ProgressObserverConfig {
            enabled: true,
            tool_call_start: true,
            ..Default::default()
        };
        assert!(cfg.any_enabled());
    }

    // -----------------------------------------------------------------
    // ProgressObserver end-to-end
    // -----------------------------------------------------------------

    #[derive(Default)]
    struct CountingObserver {
        events: AtomicUsize,
    }
    impl Observer for CountingObserver {
        fn record_event(&self, _event: &ObserverEvent) {
            self.events.fetch_add(1, Ordering::SeqCst);
        }
        fn record_metric(&self, _metric: &ObserverMetric) {}
        fn name(&self) -> &str {
            "counting"
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    #[tokio::test]
    async fn progress_observer_forwards_to_inner_and_emits_status() {
        let inner = Arc::new(CountingObserver::default());
        let (tx, mut rx) = mpsc::channel::<DraftEvent>(4);
        let obs = ProgressObserver::new(inner.clone(), Some(tx), all_on());

        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(r#"{"command": "ls"}"#.into()),
        });

        assert_eq!(inner.events.load(Ordering::SeqCst), 1, "inner must run");

        match rx.try_recv() {
            Ok(DraftEvent::Status(text)) => assert_eq!(text, "执行命令：ls"),
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn progress_observer_skips_emit_when_no_sender() {
        let inner = Arc::new(CountingObserver::default());
        let obs = ProgressObserver::new(inner.clone(), None, all_on());
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(r#"{"command": "ls"}"#.into()),
        });
        assert_eq!(
            inner.events.load(Ordering::SeqCst),
            1,
            "inner still runs without sender"
        );
    }

    #[tokio::test]
    async fn progress_observer_skips_emit_for_irrelevant_events() {
        let inner = Arc::new(CountingObserver::default());
        let (tx, mut rx) = mpsc::channel::<DraftEvent>(4);
        let obs = ProgressObserver::new(inner.clone(), Some(tx), all_on());
        obs.record_event(&ObserverEvent::HeartbeatTick);
        assert!(rx.try_recv().is_err(), "HeartbeatTick must not emit status");
        assert_eq!(inner.events.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn progress_observer_drops_status_when_channel_full() {
        let inner = Arc::new(CountingObserver::default());
        let (tx, rx) = mpsc::channel::<DraftEvent>(1);
        // Saturate the channel
        tx.try_send(DraftEvent::Status("filler".into()))
            .expect("filler send");

        let obs = ProgressObserver::new(inner.clone(), Some(tx), all_on());
        // Must not panic, must not block.
        obs.record_event(&ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(r#"{"command": "ls"}"#.into()),
        });
        assert_eq!(inner.events.load(Ordering::SeqCst), 1);
        drop(rx);
    }
}
