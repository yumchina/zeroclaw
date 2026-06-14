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
}

impl CreateTaskTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
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

        let task = resolve_executor(&self.config, task_type)
            .ok_or_else(|| anyhow::anyhow!("未配置 type={} 的 Dawn 任务", task_type))?;

        let ctx = read_context();
        if ctx.channel_alias.is_empty() {
            anyhow::bail!("dawn_create_task 必须在 DawnIM 渠道会话中调用（TASK_CONTEXT 未注入）");
        }
        let la_id = resolve_la_id(&self.config, &ctx.channel_alias).ok_or_else(|| {
            anyhow::anyhow!(
                "DawnIM 渠道别名 \"{}\" 未配置，无法解析当前机器人的 UID",
                ctx.channel_alias
            )
        })?;
        let reply_target = if ctx.reply_target.is_empty() || ctx.reply_target.ends_with(':') {
            format!("1:{}", ctx.from_uid)
        } else {
            ctx.reply_target.clone()
        };

        let user_text = args.get("user_text").and_then(|v| v.as_str()).unwrap_or("");
        let params = args.get("params").cloned().unwrap_or(serde_json::Value::Null);

        let payload = json!({
            "type": 2000,
            "cmd": "dawn.create_task",
            "param": {
                "type": task_type,
                "user_id": ctx.from_uid,
                "user_text": user_text,
                "params": params,
                "reply_to": la_id,
                "reply_target": reply_target,
            }
        });

        let msg = TaskMessage {
            channel_alias: ctx.channel_alias.clone(),
            recipient: task.recipient.clone(),
            channel_type: 1,
            payload,
        };

        bridge_sender()
            .ok_or_else(|| anyhow::anyhow!("Dawn 任务通道未初始化（CHANNEL_BRIDGE 未配置）"))?
            .send(msg)
            .map_err(|e| anyhow::anyhow!("发送任务到 Dawn 失败：{}", e))?;

        Ok(ToolResult {
            success: true,
            output: format!("已提交任务到 {}，等待处理，完成后会主动通知您", task.name),
            error: None,
        })
    }
}

// ── QueryTaskTool ──────────────────────────────────────────────────

/// Tool: query the status of a previously-submitted Dawn task.
pub struct QueryTaskTool {
    config: Arc<Config>,
}

impl QueryTaskTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
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
            .ok_or_else(|| anyhow::anyhow!("缺少 task_id 参数"))?;

        let task = resolve_executor(&self.config, task_type)
            .ok_or_else(|| anyhow::anyhow!("未配置 type={} 的 Dawn 任务", task_type))?;

        let ctx = read_context();
        if ctx.channel_alias.is_empty() {
            anyhow::bail!("dawn_query_task 必须在 DawnIM 渠道会话中调用（TASK_CONTEXT 未注入）");
        }
        let la_id = resolve_la_id(&self.config, &ctx.channel_alias).ok_or_else(|| {
            anyhow::anyhow!(
                "DawnIM 渠道别名 \"{}\" 未配置，无法解析当前机器人的 UID",
                ctx.channel_alias
            )
        })?;

        let payload = json!({
            "type": 2000,
            "cmd": "dawn.query_task",
            "param": {
                "type": task_type,
                "task_id": task_id,
                "user_id": ctx.from_uid,
                "reply_to": la_id,
            }
        });

        let msg = TaskMessage {
            channel_alias: ctx.channel_alias.clone(),
            recipient: task.recipient.clone(),
            channel_type: 1,
            payload,
        };

        bridge_sender()
            .ok_or_else(|| anyhow::anyhow!("Dawn 任务通道未初始化（CHANNEL_BRIDGE 未配置）"))?
            .send(msg)
            .map_err(|e| anyhow::anyhow!("发送查询到 Dawn 失败：{}", e))?;

        Ok(ToolResult {
            success: true,
            output: format!("已发送查询请求，task_id: {}", task_id),
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
    async fn create_task_errors_without_context() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let tool = CreateTaskTool::new(cfg);
        // Don't scope TASK_CONTEXT — execute should bail.
        let result = tool
            .execute(json!({"type": 1, "user_text": "hi", "params": {}}))
            .await
            .unwrap_err();
        assert!(result.to_string().contains("TASK_CONTEXT"));
    }

    #[tokio::test]
    async fn create_task_unknown_type_errors() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let tool = CreateTaskTool::new(cfg);
        let ctx = TaskContext {
            from_uid: "u_alice".into(),
            reply_target: "1:u_alice".into(),
            channel_alias: "work".into(),
        };
        let err = TASK_CONTEXT
            .scope(ctx, async {
                tool.execute(json!({"type": 99, "user_text": "x", "params": {}}))
                    .await
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("未配置 type=99"));
    }

    #[tokio::test]
    async fn create_task_unknown_alias_errors() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let tool = CreateTaskTool::new(cfg);
        let ctx = TaskContext {
            from_uid: "u_alice".into(),
            reply_target: "1:u_alice".into(),
            channel_alias: "nonexistent_alias".into(),
        };
        let err = TASK_CONTEXT
            .scope(ctx, async {
                tool.execute(json!({"type": 1, "user_text": "x", "params": {}}))
                    .await
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("未配置"));
    }

    /// Single combined test for bridge-using tools. The bridge is a process-
    /// global static, so concurrent `#[tokio::test]`s installing different
    /// senders would race; we serialise them here. Behaviour exercised:
    /// `dawn_create_task` and `dawn_query_task` both push their payloads
    /// onto the bridge with correctly-shaped JSON and the originating
    /// channel alias.
    #[tokio::test]
    async fn create_and_query_push_payloads_via_bridge() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let (tx, mut rx) = mpsc::unbounded_channel();
        set_channel_bridge(tx);

        let create = CreateTaskTool::new(cfg.clone());
        let ctx = TaskContext {
            from_uid: "u_alice".into(),
            reply_target: "1:u_alice".into(),
            channel_alias: "work".into(),
        };
        let create_result = TASK_CONTEXT
            .scope(ctx.clone(), async {
                create
                    .execute(json!({
                        "type": 1,
                        "user_text": "extract this pdf",
                        "params": {"files": []}
                    }))
                    .await
            })
            .await
            .unwrap();
        assert!(create_result.success);

        let msg = rx.recv().await.expect("bridge received create message");
        assert_eq!(msg.channel_alias, "work");
        assert_eq!(msg.recipient, "1878_xuanji_agent");
        assert_eq!(msg.channel_type, 1);
        assert_eq!(msg.payload["type"], 2000);
        assert_eq!(msg.payload["cmd"], "dawn.create_task");
        assert_eq!(msg.payload["param"]["type"], 1);
        assert_eq!(msg.payload["param"]["user_id"], "u_alice");
        assert_eq!(msg.payload["param"]["reply_to"], "bot_uid_1");
        assert_eq!(msg.payload["param"]["reply_target"], "1:u_alice");

        let query = QueryTaskTool::new(cfg);
        let query_result = TASK_CONTEXT
            .scope(ctx, async {
                query
                    .execute(json!({"type": 1, "task_id": "task_xxx"}))
                    .await
            })
            .await
            .unwrap();
        assert!(query_result.success);

        let msg = rx.recv().await.expect("bridge received query message");
        assert_eq!(msg.payload["cmd"], "dawn.query_task");
        assert_eq!(msg.payload["param"]["task_id"], "task_xxx");
        assert_eq!(msg.payload["param"]["reply_to"], "bot_uid_1");
    }
}
