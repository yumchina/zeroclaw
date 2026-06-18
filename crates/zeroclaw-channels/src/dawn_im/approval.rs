//! Tool-call approval flow for DawnIM.
//!
//! Combines the master `approval/mod.rs` (PendingApprovals state) and
//! `approval/card.rs` (interactive card UI builders) into a single file.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use zeroclaw_api::channel::{ChannelApprovalRequest, ChannelApprovalResponse};

use super::connection::WkMessageType;

/// Pending approvals waiting on operator response.
/// Key = (approval_id, recipient_uid), value = sender + the topic the original
/// card was sent into (so cancel_approval can land the resolved-status card in
/// the same topic thread).
pub struct PendingApproval {
    pub sender: tokio::sync::oneshot::Sender<ChannelApprovalResponse>,
    pub topic: Option<String>,
}

pub type PendingApprovals = RwLock<HashMap<(String, String), PendingApproval>>;

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalCard {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub timeout_secs: u64,
    pub title: String,
    pub body: WkApprovalBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<WkAction>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalBody {
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkAction {
    pub text: String,
    pub value: String,
    pub style: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalAction {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub action: String,
}

pub fn build_approval_card(
    approval_id: &str,
    request: &ChannelApprovalRequest,
    timeout_secs: u64,
) -> WkApprovalCard {
    let (title, content) = if request.tool_name == "cron_add" {
        let mut summary = request.arguments_summary.clone();
        summary = summary
            .replace("job_type: agent, ", "任务类型: 智能体, ")
            .replace("job_type: shell, ", "任务类型: 脚本, ")
            .replace("name: ", "任务名称: ")
            .replace("prompt: ", "提示词: ")
            .replace("command: ", "执行命令: ")
            .replace("schedule: ", "\n执行计划: ");

        let mut time_info = summary
            .split("\n执行计划: ")
            .last()
            .unwrap_or("按计划执行")
            .to_string();
        if time_info.contains("\"at\":")
            && let Some(start) = time_info.find("\"at\":\"")
        {
            let rest = &time_info[start + 6..];
            if let Some(end) = rest.find('"') {
                time_info = rest[..end].replace('T', " ").replace('Z', " (UTC)");
            }
        }
        (
            "📋 任务执行审批",
            format!(
                "1. **执行的是什么**\n添加定时任务: **{}**\n\n2. **执行的时间相关信息**\n{}\n\n3. **执行内容的总结**\n{}",
                request.tool_name, time_info, summary
            ),
        )
    } else {
        (
            "📋 任务执行审批",
            format!(
                "🔧 智能体请求执行: **{}**\n\n**执行内容总结**:\n{}",
                request.tool_name, request.arguments_summary
            ),
        )
    };

    WkApprovalCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        approval_id: approval_id.to_string(),
        timeout_secs,
        title: title.to_string(),
        body: WkApprovalBody {
            content: content.to_string(),
        },
        actions: Some(vec![
            WkAction {
                text: "同意".to_string(),
                value: "approve".to_string(),
                style: "primary".to_string(),
            },
            WkAction {
                text: "始终允许".to_string(),
                value: "always".to_string(),
                style: "primary".to_string(),
            },
            WkAction {
                text: "拒绝".to_string(),
                value: "deny".to_string(),
                style: "danger".to_string(),
            },
        ]),
    }
}

/// Render a no-button "resolved-status" card to replace an in-flight approval
/// card after another superuser already decided. Used by
/// `Channel::cancel_approval`. The `reason` argument is a pre-localized
/// human-facing string (the broker resolves fluent keys before passing it in).
pub fn build_resolved_card(approval_id: &str, reason: &str) -> WkApprovalCard {
    WkApprovalCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        approval_id: approval_id.to_string(),
        timeout_secs: 0,
        title: "📋 任务执行审批".to_string(),
        body: WkApprovalBody {
            content: reason.to_string(),
        },
        actions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(tool: &str, summary: &str) -> ChannelApprovalRequest {
        ChannelApprovalRequest {
            tool_name: tool.to_string(),
            arguments_summary: summary.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn card_has_type_20() {
        let card = build_approval_card("id1", &req("shell_exec", "cmd: ls"), 300);
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("\"type\":20"));
    }

    #[test]
    fn card_has_approve_and_deny_actions() {
        let card = build_approval_card("id2", &req("shell_exec", "cmd: echo"), 60);
        let actions = card.actions.unwrap();
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].value, "approve");
        assert_eq!(actions[1].value, "always");
        assert_eq!(actions[2].value, "deny");
    }

    #[test]
    fn cron_add_card_localizes_job_type() {
        let card = build_approval_card(
            "id3",
            &req(
                "cron_add",
                "job_type: agent, name: daily, schedule: 0 9 * * *",
            ),
            300,
        );
        assert!(card.body.content.contains("智能体"));
        assert!(card.body.content.contains("daily"));
    }

    #[test]
    fn approval_action_deny_deserializes() {
        let json = r#"{"type":21,"approval_id":"id1","action":"deny"}"#;
        let a: WkApprovalAction = serde_json::from_str(json).unwrap();
        assert_eq!(a.action, "deny");
        assert_eq!(a.msg_type, 21);
    }

    #[test]
    fn card_has_three_buttons_including_always() {
        let card = build_approval_card("id-X", &req("shell", "cmd: ls"), 300);
        let actions = card.actions.expect("actions");
        let values: Vec<&str> = actions.iter().map(|a| a.value.as_str()).collect();
        assert_eq!(values, vec!["approve", "always", "deny"]);
    }

    #[test]
    fn approval_action_always_deserializes() {
        let json = r#"{"type":21,"approval_id":"id1","action":"always"}"#;
        let a: WkApprovalAction = serde_json::from_str(json).unwrap();
        assert_eq!(a.action, "always");
    }

    #[test]
    fn resolved_card_renders_reason_as_body() {
        let card = build_resolved_card("id-X", "此请求已被处理 — 同意");
        assert!(card.actions.is_none());
        assert_eq!(card.body.content, "此请求已被处理 — 同意");
    }

    #[test]
    fn action_always_maps_to_always_approve() {
        // The mapping logic lives in channel::map_approval_action.
        // This test pins the contract that "always" must NOT fall through to default-deny.
        let json = r#"{"type":21,"approval_id":"id-Y","action":"always"}"#;
        let act: WkApprovalAction = serde_json::from_str(json).unwrap();
        let mapped = crate::dawn_im::channel::map_approval_action(&act);
        assert!(matches!(
            mapped,
            zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove
        ));
    }
}
