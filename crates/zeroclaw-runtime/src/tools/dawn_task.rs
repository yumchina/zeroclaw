//! Built-in tools for Dawn Agent task submission.
//!
//! Two tools: `dawn_create_task` and `dawn_query_task`.
//! Both send CMD messages (type=2000) through a global mpsc bridge to the
//! WuKongIM channel supervisor, which forwards them to the appropriate agent
//! based on the task type configuration.

use async_trait::async_trait;
use serde_json::json;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::dawn_agents::DawnAgents;

// ── Task-local user context ────────────────────────────────────────
// Uses tokio::task_local! so the context follows the task across thread
// migrations, unlike std::thread_local! which is per-OS-thread.
// The orchestrator wraps each agent turn in DAWN_CONTEXT.scope().

#[derive(Clone, Default)]
pub struct DawnContext {
    pub from_uid: String,
    pub reply_target: String,
    pub topic: Option<String>,
}

tokio::task_local! {
    pub static DAWN_CONTEXT: DawnContext;
}

fn read_context() -> DawnContext {
    DAWN_CONTEXT.try_with(|c| c.clone()).unwrap_or_default()
}

// ── Global mpsc bridge ──────────────────────────────────────────
// Follows CRON_CHANNEL_REGISTRY pattern (OnceLock + daemon injection).
// The bridge sender is set by the daemon after mpsc channel creation.
// The orchestrator's bridge listener consumes the receiver and calls
// WuKongIMChannel::send_status_message_with_topic().

#[derive(Debug, Clone)]
pub struct DawnMsg {
    pub recipient: String,
    pub channel_type: u8,
    pub payload: serde_json::Value,
    pub topic: Option<String>,
}

type DawnBridgeTx = mpsc::UnboundedSender<DawnMsg>;

static DAWN_BRIDGE: OnceLock<DawnBridgeTx> = OnceLock::new();

/// Set the global bridge sender. Called once by the daemon during startup.
/// Subsequent calls are silently ignored (OnceLock semantics).
pub fn set_dawn_bridge(tx: DawnBridgeTx) {
    let _ = DAWN_BRIDGE.set(tx);
}

// ── DawnCreateTask ───────────────────────────────────────────────

pub struct DawnCreateTask {
    la_id: String,
    dawn_agents: DawnAgents,
}

impl DawnCreateTask {
    pub fn new(la_id: String, dawn_agents: DawnAgents) -> Self {
        Self { la_id, dawn_agents }
    }
}

#[async_trait]
impl Tool for DawnCreateTask {
    fn name(&self) -> &str {
        "dawn_create_task"
    }

    fn description(&self) -> &str {
        "向 Dawn 平台 Agent 提交任务。根据 type 参数选择目标 Agent，通过 WuKongIM 发送任务消息。\
         当前支持：1=文档提取, 2=代码分析, 3=数据处理。\
         重要：params 结构因 type 而异。比如：type=1 时 params 必须包含 files 数组：\
         {\"files\": [{\"file_url\": \"<下载URL>\", \"file_name\": \"<文件名>\", \"file_type\": \"<类型>\"}]}. \
         请严格参考对应 Agent 的技能文档。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "type": {
                    "type": "integer",
                    "description": "任务类型：1=文档提取, 2=代码分析, 3=数据处理",
                    "enum": [1, 2, 3]
                },
                "user_text": {
                    "type": "string",
                    "description": "用户原始文字描述"
                },
                "params": {
                    "type": "object",
                    "description": "Agent 自定义参数，不同 type 对应不同结构"
                }
            },
            "required": ["type", "user_text", "params"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // 1. 获取 type 参数
        let task_type = args["type"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("缺少 type 参数"))? as u8;

        // 2. 从配置获取 agent_uid
        let agent_config = self
            .dawn_agents
            .get_by_type(task_type)
            .ok_or_else(|| anyhow::anyhow!("未知的任务类型: {}", task_type))?;
        let agent_uid = agent_config.uid.clone();

        // 3. 构造消息
        let user_text = args["user_text"].as_str().unwrap_or_default();
        let params = &args["params"];
        let ctx = read_context();
        let user_id = ctx.from_uid;
        let reply_target = if ctx.reply_target.is_empty() || ctx.reply_target.ends_with(':') {
            format!("1:{}", user_id)
        } else {
            ctx.reply_target.clone()
        };
        let topic = ctx.topic.filter(|t| !t.is_empty() && t != "0");

        let mut param = serde_json::json!({
            "type": task_type,
            "user_id": user_id,
            "user_text": user_text,
            "params": params,
            "reply_to": self.la_id,
            "reply_target": reply_target
        });
        if let Some(ref t) = topic {
            param["topic"] = serde_json::Value::String(t.clone());
        }

        let payload = serde_json::json!({
            "type": 2000,
            "cmd": "dawn.create_task",
            "param": param
        });

        // 4. 发送
        DAWN_BRIDGE
            .get()
            .ok_or_else(|| anyhow::anyhow!("Dawn 桥接未配置（DAWN_BRIDGE 未设置）"))?
            .send(DawnMsg {
                recipient: agent_uid,
                channel_type: 1,
                payload,
                topic,
            })
            .map_err(|e| anyhow::anyhow!("发送消息到 Dawn Agent 失败: {}", e))?;

        Ok(ToolResult {
            success: true,
            output: format!(
                "已提交任务到 {}，需要等待处理，完成后会主动通知您",
                agent_config.name
            ),
            error: None,
        })
    }
}

// ── DawnQueryTask ────────────────────────────────────────────────

pub struct DawnQueryTask {
    la_id: String,
    dawn_agents: DawnAgents,
}

impl DawnQueryTask {
    pub fn new(la_id: String, dawn_agents: DawnAgents) -> Self {
        Self { la_id, dawn_agents }
    }
}

#[async_trait]
impl Tool for DawnQueryTask {
    fn name(&self) -> &str {
        "dawn_query_task"
    }

    fn description(&self) -> &str {
        "查询 Dawn 平台任务的进度和结果。传入任务类型和 task_id，返回当前状态。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "type": {
                    "type": "integer",
                    "description": "任务类型：1=文档提取, 2=代码分析, 3=数据处理",
                    "enum": [1, 2, 3]
                },
                "task_id": {
                    "type": "string",
                    "description": "创建任务时返回的 task_id"
                }
            },
            "required": ["type", "task_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_type = args["type"]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("缺少 type 参数"))? as u8;
        let task_id = args["task_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("缺少 task_id 参数"))?;

        let agent_config = self
            .dawn_agents
            .get_by_type(task_type)
            .ok_or_else(|| anyhow::anyhow!("未知的任务类型: {}", task_type))?;
        let agent_uid = agent_config.uid.clone();

        let ctx = read_context();
        let topic = ctx.topic.filter(|t| !t.is_empty() && t != "0");

        let mut param = serde_json::json!({
            "type": task_type,
            "task_id": task_id,
            "user_id": ctx.from_uid,
            "reply_to": self.la_id
        });
        if let Some(ref t) = topic {
            param["topic"] = serde_json::Value::String(t.clone());
        }

        let payload = serde_json::json!({
            "type": 2000,
            "cmd": "dawn.query_task",
            "param": param
        });

        DAWN_BRIDGE
            .get()
            .ok_or_else(|| anyhow::anyhow!("Dawn 桥接未配置（DAWN_BRIDGE 未设置）"))?
            .send(DawnMsg {
                recipient: agent_uid,
                channel_type: 1,
                payload,
                topic,
            })
            .map_err(|e| anyhow::anyhow!("发送查询到 Dawn Agent 失败: {}", e))?;

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
    use std::collections::HashMap;
    use zeroclaw_config::dawn_agents::{DawnAgentConfig, DawnAgents};

    fn dawn_agents() -> DawnAgents {
        DawnAgents {
            agents: HashMap::from([(
                "1".to_string(),
                DawnAgentConfig {
                    uid: "xuanji_worker".to_string(),
                    name: "xuanji".to_string(),
                    description: "doc extraction".to_string(),
                },
            )]),
        }
    }

    #[tokio::test]
    async fn create_task_forwards_current_topic_to_bridge_and_payload() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        set_dawn_bridge(tx);

        let tool = DawnCreateTask::new("la_uid".to_string(), dawn_agents());
        let args = json!({
            "type": 1,
            "user_text": "extract this file",
            "params": {
                "files": [{
                    "file_url": "https://example.invalid/a.pdf",
                    "file_name": "a.pdf",
                    "file_type": "pdf"
                }]
            }
        });
        let ctx = DawnContext {
            from_uid: "user_1".to_string(),
            reply_target: "1:user_1".to_string(),
            topic: Some("topic-123".to_string()),
        };

        let result = DAWN_CONTEXT.scope(ctx, tool.execute(args)).await.unwrap();

        assert!(result.success);
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.recipient, "xuanji_worker");
        assert_eq!(msg.channel_type, 1);
        assert_eq!(msg.topic.as_deref(), Some("topic-123"));
        assert_eq!(msg.payload["param"]["topic"], "topic-123");
    }
}
