//! Built-in tools for 璇玑Agent (Xuanji) document extraction.
//!
//! Two tools: `dawn_xuanji_create_task` and `dawn_xuanji_query_task`.
//! Both send CMD messages (type=2000) through a global mpsc bridge to the
//! WuKongIM channel supervisor, which forwards them to 璇玑Agent.

use async_trait::async_trait;
use serde_json::json;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use zeroclaw_api::tool::{Tool, ToolResult};

// ── Task-local user context ────────────────────────────────────────
// Uses tokio::task_local! so the context follows the task across thread
// migrations, unlike std::thread_local! which is per-OS-thread.
// The orchestrator wraps each agent turn in XUANJI_CONTEXT.scope().

#[derive(Clone, Default)]
pub struct XuanjiContext {
    pub from_uid: String,
    pub reply_target: String,
}

tokio::task_local! {
    pub static XUANJI_CONTEXT: XuanjiContext;
}

fn read_context() -> XuanjiContext {
    XUANJI_CONTEXT.try_with(|c| c.clone()).unwrap_or_default()
}

// ── Global mpsc bridge ──────────────────────────────────────────
// Follows CRON_CHANNEL_REGISTRY pattern (OnceLock + daemon injection).
// The bridge sender is set by the daemon after mpsc channel creation.
// The orchestrator's bridge listener consumes the receiver and calls
// WuKongIMChannel::send_status_message().

type XuanjiMsg = (String, u8, serde_json::Value); // (recipient, channel_type, payload)
type XuanjiBridgeTx = mpsc::UnboundedSender<XuanjiMsg>;

static XUANJI_BRIDGE: OnceLock<XuanjiBridgeTx> = OnceLock::new();

/// Set the global bridge sender. Called once by the daemon during startup.
/// Subsequent calls are silently ignored (OnceLock semantics).
pub fn set_xuanji_bridge(tx: XuanjiBridgeTx) {
    let _ = XUANJI_BRIDGE.set(tx);
}

// ── DawnXuanjiCreateTask ────────────────────────────────────────

pub struct DawnXuanjiCreateTask {
    la_id: String,
}

impl DawnXuanjiCreateTask {
    pub fn new(la_id: String) -> Self {
        Self { la_id }
    }
}

#[async_trait]
impl Tool for DawnXuanjiCreateTask {
    fn name(&self) -> &str {
        "dawn_xuanji_create_task"
    }

    fn description(&self) -> &str {
        "向璇玑Agent 提交文档内容提取任务。支持 pdf/docx/pptx/xlsx 格式，一次可提交多个文件。\
         传入文件的下载链接、文件名、文件类型和用户原文，返回 execution_id。\
         提交后璇玑Agent 异步处理，完成后会主动推送结果。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "xuanji_uid": {
                    "type": "string",
                    "description": "璇玑机器人在 WuKongIM 中的 UID"
                },
                "user_text": {
                    "type": "string",
                    "description": "用户发送文件时的原始文字消息"
                },
                "files": {
                    "type": "array",
                    "description": "文件列表，每个元素为 {\"file_url\": \"...\", \"file_name\": \"...\", \"file_type\": \"pdf|docx|pptx|xlsx\"}",
                    "items": {
                        "type": "object",
                        "properties": {
                            "file_url": {"type": "string", "description": "文件的下载链接（从消息中的 <!-- file-url: ... --> 注释获取）"},
                            "file_name": {"type": "string", "description": "原始文件名"},
                            "file_type": {"type": "string", "description": "文件类型：pdf/docx/pptx/xlsx"}
                        }
                    }
                }
            },
            "required": ["xuanji_uid", "user_text", "files"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let bridge = XUANJI_BRIDGE
            .get()
            .ok_or_else(|| anyhow::anyhow!("璇玑Agent 桥接未配置（XUANJI_BRIDGE 未设置）"))?;

        let xuanji_uid = args["xuanji_uid"].as_str()
            .ok_or_else(|| anyhow::anyhow!("缺少 xuanji_uid 参数"))?;
        let user_text = args["user_text"].as_str().unwrap_or_default();
        let files = &args["files"];
        let ctx = read_context();
        let user_id = ctx.from_uid;
        let reply_target = if ctx.reply_target.is_empty() || ctx.reply_target.ends_with(':') {
            format!("1:{}", user_id)
        } else {
            ctx.reply_target
        };

        if files.as_array().map_or(true, |a| a.is_empty()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("files 参数不能为空".to_string()),
            });
        }

        let payload = json!({
            "type": 2000,
            "cmd": "xuanji.create_extraction_task",
            "param": {
                "user_id": user_id,
                "reply_to": self.la_id,
                "reply_target": reply_target,
                "user_text": user_text,
                "files": files
            }
        });

        bridge
            .send((xuanji_uid.to_string(), 1, payload))
            .map_err(|e| anyhow::anyhow!("发送消息到璇玑Agent 失败: {e}"))?;

        let file_count = files.as_array().map(|a| a.len()).unwrap_or(0);
        Ok(ToolResult {
            success: true,
            output: format!(
                "已提交 {} 个文件的提取任务，需要等待一段时间，完成后会主动通知您",
                file_count
            ),
            error: None,
        })
    }
}

// ── DawnXuanjiQueryTask ─────────────────────────────────────────

pub struct DawnXuanjiQueryTask {
    la_id: String,
}

impl DawnXuanjiQueryTask {
    pub fn new(la_id: String) -> Self {
        Self { la_id }
    }
}

#[async_trait]
impl Tool for DawnXuanjiQueryTask {
    fn name(&self) -> &str {
        "dawn_xuanji_query_task"
    }

    fn description(&self) -> &str {
        "查询璇玑Agent 文档提取任务的进度和结果。\
         传入 task_id，返回当前状态（pending/running/completed/failed）和结果内容。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "xuanji_uid": {
                    "type": "string",
                    "description": "璇玑机器人在 WuKongIM 中的 UID"
                },
                "task_id": {
                    "type": "string",
                    "description": "创建任务时返回的 task_id"
                }
            },
            "required": ["xuanji_uid", "task_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let bridge = XUANJI_BRIDGE
            .get()
            .ok_or_else(|| anyhow::anyhow!("璇玑Agent 桥接未配置（XUANJI_BRIDGE 未设置）"))?;

        let xuanji_uid = args["xuanji_uid"].as_str()
            .ok_or_else(|| anyhow::anyhow!("缺少 xuanji_uid 参数"))?;
        let task_id = args["task_id"].as_str().unwrap_or_default();
        let ctx = read_context();
        let user_id = ctx.from_uid;

        let payload = json!({
            "type": 2000,
            "cmd": "xuanji.query_extraction_task",
            "param": {
                "user_id": user_id,
                "task_id": task_id,
                "reply_to": self.la_id
            }
        });

        bridge
            .send((xuanji_uid.to_string(), 1, payload))
            .map_err(|e| anyhow::anyhow!("发送查询到璇玑Agent 失败: {e}"))?;

        Ok(ToolResult {
            success: true,
            output: format!("已发送查询请求，task_id: {}", task_id),
            error: None,
        })
    }
}
