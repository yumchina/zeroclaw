//! Built-in tools for 璇玑Agent (Xuanji) document extraction.
//!
//! Two tools: `xuanji_doc_create_task` and `xuanji_doc_query_task`.
//! Both send CMD messages (type=99) through a global mpsc bridge to the
//! WuKongIM channel supervisor, which forwards them to 璇玑Agent.

use std::cell::RefCell;
use std::sync::OnceLock;

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::mpsc;
use zeroclaw_api::tool::{Tool, ToolResult};

// ── Thread-local user context ────────────────────────────────────
// The orchestrator sets the current from_uid before each agent turn.
// Xuanji tools read it during execute() to include in CMD messages.

thread_local! {
    static CURRENT_FROM_UID: RefCell<Option<String>> = RefCell::new(None);
    static CURRENT_REPLY_TARGET: RefCell<Option<String>> = RefCell::new(None);
}

/// Set the current user context (called by orchestrator before agent turn).
/// Parses WK UID format: `102535169_la_1779364164516` → `102535169`.
/// If the UID does not contain `_la_`, uses the original value as-is.
pub fn set_current_from_uid(uid: Option<&str>) {
    let parsed = uid.and_then(|u| u.split("_la_").next());
    CURRENT_FROM_UID.with(|c| *c.borrow_mut() = parsed.map(String::from));
}

/// Set the current reply target (called by orchestrator before agent turn).
pub fn set_current_reply_target(target: Option<&str>) {
    CURRENT_REPLY_TARGET.with(|c| *c.borrow_mut() = target.map(String::from));
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

// ── XuanjiCreateTaskTool ────────────────────────────────────────

pub struct XuanjiCreateTaskTool {
    xuanji_wk_uid: String,
    la_id: String,
}

impl XuanjiCreateTaskTool {
    pub fn new(xuanji_wk_uid: String, la_id: String) -> Self {
        Self {
            xuanji_wk_uid,
            la_id,
        }
    }
}

#[async_trait]
impl Tool for XuanjiCreateTaskTool {
    fn name(&self) -> &str {
        "xuanji_doc_create_task"
    }

    fn description(&self) -> &str {
        "向璇玑Agent 提交文档内容提取任务。支持 pdf/docx/pptx/xlsx 格式，一次可提交多个文件。\
         传入文件的 S3 URL、文件名、文件类型和用户原文，返回 execution_id。\
         提交后璇玑Agent 异步处理，完成后会主动推送结果。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
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
                            "file_url": {"type": "string", "description": "文件的 S3 URL"},
                            "file_name": {"type": "string", "description": "原始文件名"},
                            "file_type": {"type": "string", "description": "文件类型：pdf/docx/pptx/xlsx"}
                        }
                    }
                }
            },
            "required": ["user_text", "files"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let bridge = XUANJI_BRIDGE
            .get()
            .ok_or_else(|| anyhow::anyhow!("璇玑Agent 桥接未配置（XUANJI_BRIDGE 未设置）"))?;

        let user_text = args["user_text"].as_str().unwrap_or_default();
        let files = &args["files"];
        let user_id = CURRENT_FROM_UID.with(|c| c.borrow().clone()).unwrap_or_default();
        let reply_target = CURRENT_REPLY_TARGET.with(|c| c.borrow().clone()).unwrap_or_default();

        if files.as_array().map_or(true, |a| a.is_empty()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("files 参数不能为空".to_string()),
            });
        }

        let payload = json!({
            "type": 99,
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
            .send((self.xuanji_wk_uid.clone(), 1, payload))
            .map_err(|e| anyhow::anyhow!("发送消息到璇玑Agent 失败: {e}"))?;

        let file_count = files.as_array().map(|a| a.len()).unwrap_or(0);
        Ok(ToolResult {
            success: true,
            output: format!(
                "已提交 {} 个文件的提取任务，预计 30-60 秒完成，完成后会主动通知您",
                file_count
            ),
            error: None,
        })
    }
}

// ── XuanjiQueryTaskTool ─────────────────────────────────────────

pub struct XuanjiQueryTaskTool {
    xuanji_wk_uid: String,
    la_id: String,
}

impl XuanjiQueryTaskTool {
    pub fn new(xuanji_wk_uid: String, la_id: String) -> Self {
        Self { xuanji_wk_uid, la_id }
    }
}

#[async_trait]
impl Tool for XuanjiQueryTaskTool {
    fn name(&self) -> &str {
        "xuanji_doc_query_task"
    }

    fn description(&self) -> &str {
        "查询璇玑Agent 文档提取任务的进度和结果。\
         传入 task_id，返回当前状态（pending/running/completed/failed）和结果内容。"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "创建任务时返回的 task_id"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let bridge = XUANJI_BRIDGE
            .get()
            .ok_or_else(|| anyhow::anyhow!("璇玑Agent 桥接未配置（XUANJI_BRIDGE 未设置）"))?;

        let task_id = args["task_id"].as_str().unwrap_or_default();
        let user_id = CURRENT_FROM_UID.with(|c| c.borrow().clone()).unwrap_or_default();

        let payload = json!({
            "type": 99,
            "cmd": "xuanji.query_extraction_task",
            "param": {
                "user_id": user_id,
                "task_id": task_id,
                "reply_to": self.la_id
            }
        });

        bridge
            .send((self.xuanji_wk_uid.clone(), 1, payload))
            .map_err(|e| anyhow::anyhow!("发送查询到璇玑Agent 失败: {e}"))?;

        Ok(ToolResult {
            success: true,
            output: format!("已发送查询请求，task_id: {}", task_id),
            error: None,
        })
    }
}
