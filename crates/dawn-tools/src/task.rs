//! Dawn task submission tools (`dawn_create_task`, `dawn_query_task`).
//!
//! These tools let the assistant hand off long-running work — document
//! extraction, code analysis, data processing — to specialised Agents on the
//! Dawn platform. Tasks are dispatched via a one-way bridge into the DawnIM
//! channel supervisor, which forwards them as CMD (`type=2000`) messages over
//! WebSocket to the target Agent. Results come back asynchronously as new
//! inbound DawnIM CMD messages routed through the normal channel inbound path.
//!
//! ## Architecture
//!
//! - **Bridge** — [`CHANNEL_BRIDGE`] holds an mpsc sender swapped in by the
//!   channel supervisor at startup (and refreshed on `/admin/reload`).
//!   Tools push [`TaskMessage`]s onto it without holding any channel
//!   reference; a separate listener task owns the receiver and dispatches
//!   to the right `DawnIMChannel` instance.
//! - **Routing** — every message carries the originating `channel_alias`,
//!   so the listener can pick the correct `DawnIMChannel` in multi-instance
//!   deployments.
//! - **Context** — [`TASK_CONTEXT`] task-local carries the originating
//!   user UID, reply target, and channel alias from the orchestrator into
//!   the tool's `execute` body.
//!
//! Reply routing back to the user is handled by the receiving Agent on the
//! Dawn side: it sends a `xxx.task_complete` CMD message back to the
//! originating bot, which the orchestrator routes through the user's
//! existing session.

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::mpsc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;
use zeroclaw_config::dawn_task::DawnTaskExecutorConfig;
use zeroclaw_config::schema::Config;

// ── Bridge primitives ──────────────────────────────────────────────

/// One message handed from a tool to the channel supervisor's bridge
/// listener.
///
/// The `channel_alias` field is **required** in 0.8.0 because multiple
/// DawnIM instances may be configured; the listener uses it to look up the
/// concrete `DawnIMChannel` to forward through.
#[derive(Debug, Clone)]
pub struct TaskMessage {
    /// DawnIM alias (`[channels.dawnim.<alias>]`) that should forward this
    /// message. Sourced from the originating [`TaskContext`].
    pub channel_alias: String,
    /// Target Agent's DawnIM UID (e.g. `"1878_xuanji_agent"`).
    pub recipient: String,
    /// DawnIM channel type: `1` = personal/DM, `2` = group.
    pub channel_type: u8,
    /// Full message payload (typically `{type:2000, cmd:"...", param:{...}}`).
    pub payload: serde_json::Value,
}

/// Global bridge sender. `None` until `set_channel_bridge` is called by the
/// channel supervisor. Using `parking_lot::RwLock<Option<Sender>>` (rather
/// than `OnceLock`) so the supervisor can swap in a fresh sender on
/// `/admin/reload` without leaking a closed channel.
static CHANNEL_BRIDGE: RwLock<Option<mpsc::UnboundedSender<TaskMessage>>> =
    RwLock::new(None);

/// Install (or replace) the bridge sender. Called once by the channel
/// supervisor during startup, and again on every `/admin/reload` because
/// the mpsc receiver is owned by a per-supervisor listener task.
pub fn set_channel_bridge(tx: mpsc::UnboundedSender<TaskMessage>) {
    *CHANNEL_BRIDGE.write() = Some(tx);
}

/// Take a cheap clone of the current sender. Returns `None` when no
/// supervisor is running (e.g. CLI-only builds, or during shutdown).
fn bridge_sender() -> Option<mpsc::UnboundedSender<TaskMessage>> {
    CHANNEL_BRIDGE.read().clone()
}

// ── Task-local user context ────────────────────────────────────────

/// Per-turn context populated by the orchestrator before invoking the agent
/// loop. Carries the originating user identity and the channel alias the
/// task should reply through.
#[derive(Clone, Default, Debug)]
pub struct TaskContext {
    /// Originating user UID, sans any `_la_<bot_uid>` suffix.
    pub from_uid: String,
    /// Original `ChannelMessage.reply_target` (e.g. `"1:u_alice"`).
    pub reply_target: String,
    /// Alias of the DawnIM channel the user reached us on; used both for
    /// resolving the bot's own UID (`la_id`) and for routing the bridge
    /// message back through the same channel.
    pub channel_alias: String,
}

tokio::task_local! {
    pub static TASK_CONTEXT: TaskContext;
}

fn read_context() -> TaskContext {
    TASK_CONTEXT.try_with(|c| c.clone()).unwrap_or_default()
}

/// Resolve `la_id` (the bot's own DawnIM UID) from the captured config
/// snapshot by looking up the channel alias the user reached us through.
/// Returns `None` if the alias is not configured. Snapshot semantics match
/// the rest of the tool factory (other Dawn tools also cache config values
/// at registration time; `/admin/reload` rebuilds the tools registry).
fn resolve_la_id(config: &Arc<Config>, alias: &str) -> Option<String> {
    config.channels.dawnim.get(alias).map(|c| c.uid.clone())
}

/// Resolve a single executor entry from canonical config. Cloning out is
/// fine here — the entry is at most a few short strings.
fn resolve_executor(config: &Arc<Config>, task_type: u8) -> Option<DawnTaskExecutorConfig> {
    config.dawn_task.get_by_type(task_type).cloned()
}

// ── CreateTaskTool ─────────────────────────────────────────────────

/// Tool: submit a task to a Dawn-platform Agent.
///
/// Holds an `Arc<Config>` snapshot captured by `all_tools_with_runtime`
/// at registration time; the snapshot is rebuilt on every `/admin/reload`
/// so config edits take effect after a reload. This mirrors the other
/// `dawn-tools` tools (`DawnS3Tool`, `DawnWebSearchTool`, `DawnCrawlTool`).
pub struct CreateTaskTool {
    config: Arc<Config>,
    /// Late-bound channel registry; populated by
    /// `orchestrator::register_channels_for_tools` at startup. Used by
    /// `execute` to look up the `Arc<dyn Channel>` named in the matching
    /// `[dawn_task.<n>].channel` config entry.
    channels: zeroclaw_api::channel::PerToolChannelHandle,
}

impl CreateTaskTool {
    pub fn new(
        config: Arc<Config>,
        channels: zeroclaw_api::channel::PerToolChannelHandle,
    ) -> Self {
        Self { config, channels }
    }
}

tool_attribution!(CreateTaskTool, ::zeroclaw_api::attribution::ToolKind::DawnTask);

#[async_trait]
impl Tool for CreateTaskTool {
    fn name(&self) -> &str {
        "dawn_create_task"
    }

    fn description(&self) -> &str {
        "向 Dawn 平台 Agent 提交任务。根据 type 参数选择目标 Agent，通过 DawnIM 发送任务消息。\
         params 结构因 type 而异，请参考对应 Agent 的技能文档。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "type": {
                    "type": "integer",
                    "description": "任务类型 ID（对应 [dawn_task.<type>] 配置项）",
                    "minimum": 1
                },
                "user_text": {
                    "type": "string",
                    "description": "用户原始文字描述"
                },
                "params": {
                    "type": "object",
                    "description": "Agent 自定义参数，结构因 type 而异"
                }
            },
            "required": ["type", "user_text", "params"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_type = args
            .get("type")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("缺少 type 参数"))? as u8;

        let executor = resolve_executor(&self.config, task_type)
            .ok_or_else(|| anyhow::anyhow!("未配置 type={} 的 dawn task", task_type))?;

        let origin = zeroclaw_api::channel::CHANNEL_ORIGIN
            .try_with(|o| o.clone())
            .unwrap_or_default();

        let channel: std::sync::Arc<dyn zeroclaw_api::channel::Channel> = {
            let map = self.channels.read();
            map.get(&executor.channel).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "channel '{}' 未注册或未启用（dawn_task type={} 配置依赖此 channel）",
                    executor.channel,
                    task_type,
                )
            })?
        };

        let user_text = args
            .get("user_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = args.get("params").cloned().unwrap_or(serde_json::Value::Null);

        let msg = zeroclaw_api::channel::SendMessage {
            recipient: executor.recipient.clone(),
            kind: zeroclaw_api::channel::SendKind::TaskSubmit {
                task_type,
                user_id: origin.from_uid,
                user_text,
                params,
            },
            ..Default::default()
        };

        channel.send(&msg).await.map_err(|e| {
            anyhow::anyhow!("通过 channel '{}' 投递任务失败：{e}", executor.channel)
        })?;

        Ok(ToolResult {
            success: true,
            output: format!("已提交任务到 {}，等待处理，完成后会主动通知您", executor.name),
            error: None,
        })
    }
}

// ── QueryTaskTool ──────────────────────────────────────────────────

/// Tool: query the status of a previously-submitted Dawn task.
pub struct QueryTaskTool {
    config: Arc<Config>,
    /// Late-bound channel registry; populated by
    /// `orchestrator::register_channels_for_tools` at startup. Used by
    /// `execute` to look up the `Arc<dyn Channel>` named in the matching
    /// `[dawn_task.<n>].channel` config entry.
    channels: zeroclaw_api::channel::PerToolChannelHandle,
}

impl QueryTaskTool {
    pub fn new(
        config: Arc<Config>,
        channels: zeroclaw_api::channel::PerToolChannelHandle,
    ) -> Self {
        Self { config, channels }
    }
}

tool_attribution!(QueryTaskTool, ::zeroclaw_api::attribution::ToolKind::DawnTask);

#[async_trait]
impl Tool for QueryTaskTool {
    fn name(&self) -> &str {
        "dawn_query_task"
    }

    fn description(&self) -> &str {
        "查询 Dawn 平台任务的进度和结果。传入任务类型与 task_id，返回当前状态。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "type": {
                    "type": "integer",
                    "description": "任务类型 ID",
                    "minimum": 1
                },
                "task_id": {
                    "type": "string",
                    "description": "创建任务时 Agent 返回的 task_id"
                }
            },
            "required": ["type", "task_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_type = args
            .get("type")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("缺少 type 参数"))? as u8;
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("缺少 task_id 参数"))?
            .to_string();

        let executor = resolve_executor(&self.config, task_type)
            .ok_or_else(|| anyhow::anyhow!("未配置 type={} 的 dawn task", task_type))?;

        let origin = zeroclaw_api::channel::CHANNEL_ORIGIN
            .try_with(|o| o.clone())
            .unwrap_or_default();

        let channel: std::sync::Arc<dyn zeroclaw_api::channel::Channel> = {
            let map = self.channels.read();
            map.get(&executor.channel).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "channel '{}' 未注册或未启用（dawn_task type={} 配置依赖此 channel）",
                    executor.channel,
                    task_type,
                )
            })?
        };

        let msg = zeroclaw_api::channel::SendMessage {
            recipient: executor.recipient.clone(),
            kind: zeroclaw_api::channel::SendKind::TaskQuery {
                task_type,
                user_id: origin.from_uid,
                task_id: task_id.clone(),
            },
            ..Default::default()
        };

        channel.send(&msg).await.map_err(|e| {
            anyhow::anyhow!("通过 channel '{}' 投递查询失败：{e}", executor.channel)
        })?;

        Ok(ToolResult {
            success: true,
            output: format!("已发送查询请求，task_id: {task_id}"),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `Config` via TOML parsing for tests. Avoids the full
    /// `Config::default()` surface (which is large) and matches how the
    /// rest of `zeroclaw-config` exercises its types.
    fn make_config_with_dawnim(alias: &str, uid: &str) -> Arc<Config> {
        let toml = format!(
            r#"
[channels.dawnim.{alias}]
ws_url = "ws://localhost:5200"
uid = "{uid}"
token = ""
device_id = "test-device"

[dawn_task.1]
channel = "dawnim.work"
recipient = "1878_xuanji_agent"
name = "璇玑"
description = "doc extraction"
"#
        );
        let cfg: Config = toml::from_str(&toml).expect("test toml parses");
        Arc::new(cfg)
    }

    fn make_empty_channel_handle() -> zeroclaw_api::channel::PerToolChannelHandle {
        std::sync::Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new()))
    }

    use std::sync::Mutex as StdMutex;
    use zeroclaw_api::channel::{Channel, ChannelMessage, PerToolChannelHandle, SendMessage};

    /// Records every SendMessage passed to its `send()`.
    struct RecordingChannel {
        name: &'static str,
        recorded: StdMutex<Vec<SendMessage>>,
    }

    impl RecordingChannel {
        fn new(name: &'static str) -> Self {
            Self { name, recorded: StdMutex::new(Vec::new()) }
        }
        fn take(&self) -> Vec<SendMessage> {
            std::mem::take(&mut *self.recorded.lock().unwrap())
        }
    }

    impl zeroclaw_api::attribution::Attributable for RecordingChannel {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::Cli,
            )
        }
        fn alias(&self) -> &str { self.name }
    }

    #[async_trait::async_trait]
    impl Channel for RecordingChannel {
        fn name(&self) -> &str { self.name }
        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.recorded.lock().unwrap().push(message.clone());
            Ok(())
        }
        async fn listen(
            &self,
            _: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn make_handle_with_channel(
        alias: &str,
        ch: Arc<RecordingChannel>,
    ) -> PerToolChannelHandle {
        let map = std::collections::HashMap::from([(alias.to_string(), ch as Arc<dyn Channel>)]);
        Arc::new(parking_lot::RwLock::new(map))
    }

    #[tokio::test]
    async fn create_task_sends_via_channel_handle() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let ch = Arc::new(RecordingChannel::new("dawnim"));
        let handle = make_handle_with_channel("dawnim.work", ch.clone());

        let tool = CreateTaskTool::new(cfg, handle);
        let origin = zeroclaw_api::channel::ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.work".into(),
            reply_target: "1:u_alice".into(),
        };
        let result = zeroclaw_api::channel::CHANNEL_ORIGIN
            .scope(origin, async {
                tool.execute(serde_json::json!({
                    "type": 1,
                    "user_text": "extract this pdf",
                    "params": {"files": []}
                }))
                .await
            })
            .await
            .unwrap();
        assert!(result.success);

        let recorded = ch.take();
        assert_eq!(recorded.len(), 1);
        let msg = &recorded[0];
        assert_eq!(msg.recipient, "1878_xuanji_agent");
        match &msg.kind {
            zeroclaw_api::channel::SendKind::TaskSubmit {
                task_type, user_id, user_text, params,
            } => {
                assert_eq!(*task_type, 1);
                assert_eq!(user_id, "u_alice");
                assert_eq!(user_text, "extract this pdf");
                assert_eq!(params["files"], serde_json::json!([]));
            }
            other => panic!("expected TaskSubmit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_task_errors_when_channel_not_in_handle() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        // 注册一个错的 alias，让 handle 查 dawnim.work 时 miss
        let ch = Arc::new(RecordingChannel::new("dawnim"));
        let handle = make_handle_with_channel("dawnim.other", ch);

        let tool = CreateTaskTool::new(cfg, handle);
        let origin = zeroclaw_api::channel::ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.other".into(),
            reply_target: "1:u_alice".into(),
        };
        let err = zeroclaw_api::channel::CHANNEL_ORIGIN
            .scope(origin, async {
                tool.execute(serde_json::json!({"type": 1, "user_text": "x", "params": {}}))
                    .await
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("dawnim.work"), "got: {err}");
    }

    #[tokio::test]
    async fn query_task_sends_via_channel_handle() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let ch = Arc::new(RecordingChannel::new("dawnim"));
        let handle = make_handle_with_channel("dawnim.work", ch.clone());

        let tool = QueryTaskTool::new(cfg, handle);
        let origin = zeroclaw_api::channel::ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.work".into(),
            reply_target: "1:u_alice".into(),
        };
        let result = zeroclaw_api::channel::CHANNEL_ORIGIN
            .scope(origin, async {
                tool.execute(serde_json::json!({"type": 1, "task_id": "task_abc"}))
                    .await
            })
            .await
            .unwrap();
        assert!(result.success);

        let recorded = ch.take();
        assert_eq!(recorded.len(), 1);
        match &recorded[0].kind {
            zeroclaw_api::channel::SendKind::TaskQuery {
                task_type, user_id, task_id,
            } => {
                assert_eq!(*task_type, 1);
                assert_eq!(user_id, "u_alice");
                assert_eq!(task_id, "task_abc");
            }
            other => panic!("expected TaskQuery, got {other:?}"),
        }
    }

    #[test]
    fn bridge_sender_none_when_unset() {
        // Use a fresh lock guard; can't reset the static, but its initial
        // state is None and nothing else in this test installs a sender.
        // Skip when something else (e.g. another test) has populated it.
        let installed_before = bridge_sender().is_some();
        if installed_before {
            return;
        }
        assert!(bridge_sender().is_none());
    }

    #[test]
    fn bridge_sender_returns_clone_after_set() {
        let (tx, _rx) = mpsc::unbounded_channel();
        set_channel_bridge(tx.clone());
        let got = bridge_sender().expect("sender installed");
        // A clone of the sender should send onto the same channel.
        assert!(!got.is_closed());
    }

    #[tokio::test]
    async fn create_task_unknown_type_errors() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let ch = Arc::new(RecordingChannel::new("dawnim"));
        let handle = make_handle_with_channel("dawnim.work", ch);
        let tool = CreateTaskTool::new(cfg, handle);
        let origin = zeroclaw_api::channel::ChannelOrigin {
            from_uid: "u_alice".into(),
            reply_target: "1:u_alice".into(),
            channel_ref: "dawnim.work".into(),
        };
        let err = zeroclaw_api::channel::CHANNEL_ORIGIN
            .scope(origin, async {
                tool.execute(json!({"type": 99, "user_text": "x", "params": {}}))
                    .await
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("未配置 type=99"));
    }

}
