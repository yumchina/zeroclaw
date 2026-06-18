//! The per-tool-call approval gate: CLI prompt, channel inline approval, or
//! auto-deny, plus decision recording.

use super::context::TurnCtx;
use super::events::StreamDelta;
use crate::agent::tool_execution::ToolExecutionOutcome;
use crate::approval::{
    ApprovalRequest, ApprovalRequirement, ApprovalResponse, GrantLookupCtx, broker::BrokerDecision,
    broker::BrokerRequestCtx, decision_reason::*,
};
use std::time::Duration;

/// Outcome of [`gate_tool_approval`] for one tool call.
///
/// `Deny`/`Replace` carry the synthesized [`ToolExecutionOutcome`] the caller
/// records into its `ordered_results` slot before skipping execution;
/// `Proceed::approved` feeds `set_runtime_approved_arg`.
pub(crate) enum ApprovalGateOutcome {
    Proceed { approved: bool },
    Deny(ToolExecutionOutcome),
    Replace(ToolExecutionOutcome),
}

/// Read the current `ChannelOrigin` from the task-local scope.
fn read_origin() -> zeroclaw_api::channel::ChannelOrigin {
    zeroclaw_api::channel::CHANNEL_ORIGIN
        .try_with(|o| o.clone())
        .unwrap_or_default()
}

/// Map `ApprovalResponse` → decision reason constant.
fn cli_reason_for(decision: &ApprovalResponse) -> &'static str {
    match decision {
        ApprovalResponse::Yes => INTERACTIVE_APPROVE,
        ApprovalResponse::Always => INTERACTIVE_ALWAYS,
        ApprovalResponse::No => INTERACTIVE_DENY,
        ApprovalResponse::ReplaceWith(_) => INTERACTIVE_REPLACE,
    }
}

/// Map CLI `ApprovalResponse` → `ApprovalGateOutcome`.
fn cli_decision_to_outcome(decision: ApprovalResponse, _tool_name: &str) -> ApprovalGateOutcome {
    match decision {
        ApprovalResponse::No => {
            let denied = "Denied by user.".to_string();
            ApprovalGateOutcome::Deny(ToolExecutionOutcome {
                output: denied.clone(),
                success: false,
                error_reason: Some(denied),
                duration: Duration::ZERO,
                receipt: None,
            })
        }
        ApprovalResponse::ReplaceWith(replacement) => {
            ApprovalGateOutcome::Replace(ToolExecutionOutcome {
                output: crate::approval::sanitize_tool_replacement(&replacement),
                success: true,
                error_reason: None,
                duration: Duration::ZERO,
                receipt: None,
            })
        }
        ApprovalResponse::Yes | ApprovalResponse::Always => {
            ApprovalGateOutcome::Proceed { approved: true }
        }
    }
}

/// Map broker `BrokerDecision` → `ApprovalGateOutcome`, calling
/// `mgr.record_decision` with the broker's reason and returning the outcome.
fn map_broker_decision(
    decision: BrokerDecision,
    mgr: &crate::approval::ApprovalManager,
    tool_name: &str,
    tool_args: &serde_json::Value,
    ctx: &TurnCtx<'_>,
    _iteration: usize,
) -> ApprovalGateOutcome {
    match decision {
        BrokerDecision::Approve { reason, grant_id } => {
            let extras = grant_id
                .map(|id| serde_json::json!({"grant_id": id}))
                .unwrap_or_else(|| serde_json::json!({}));
            mgr.record_decision(
                tool_name,
                tool_args,
                &ApprovalResponse::Yes,
                ctx.channel_name,
                reason,
                extras,
            );
            ApprovalGateOutcome::Proceed { approved: true }
        }
        BrokerDecision::Deny { reason } => {
            mgr.record_decision(
                tool_name,
                tool_args,
                &ApprovalResponse::No,
                ctx.channel_name,
                reason,
                serde_json::json!({}),
            );
            let denied = "Denied by user.".to_string();
            ApprovalGateOutcome::Deny(ToolExecutionOutcome {
                output: denied.clone(),
                success: false,
                error_reason: Some(denied),
                duration: Duration::ZERO,
                receipt: None,
            })
        }
        BrokerDecision::Replace {
            replacement,
            reason,
        } => {
            mgr.record_decision(
                tool_name,
                tool_args,
                &ApprovalResponse::ReplaceWith(replacement.clone()),
                ctx.channel_name,
                reason,
                serde_json::json!({}),
            );
            ApprovalGateOutcome::Replace(ToolExecutionOutcome {
                output: crate::approval::sanitize_tool_replacement(&replacement),
                success: true,
                error_reason: None,
                duration: Duration::ZERO,
                receipt: None,
            })
        }
    }
}

/// Run the approval flow for one tool call (upstream loop body, approval
/// section): resolve the tool's approval requirement, prompt interactively on
/// CLI or via the channel's inline approval on non-interactive channels
/// (falling back to auto-deny), and record the decision.
pub(crate) async fn gate_tool_approval(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
) -> ApprovalGateOutcome {
    let origin = read_origin();
    let lookup_ctx = origin
        .triggerer_master_id
        .as_ref()
        .map(|mid| GrantLookupCtx {
            channel_ref: origin.channel_ref.clone(),
            topic: origin.topic.clone(),
            user_master_id: mid.clone(),
        });

    let approval_requirement = ctx
        .approval
        .map(|mgr| mgr.approval_requirement(tool_name, lookup_ctx.as_ref()))
        .unwrap_or(ApprovalRequirement::NotRequired);

    if approval_requirement != ApprovalRequirement::Prompt {
        return ApprovalGateOutcome::Proceed {
            approved: approval_requirement == ApprovalRequirement::Approved,
        };
    }

    let Some(mgr) = ctx.approval else {
        return ApprovalGateOutcome::Proceed { approved: false };
    };

    // Broker path: if broker is attached, route through it.
    if let Some(broker) = mgr.broker() {
        let req_ctx = BrokerRequestCtx {
            tool_name,
            tool_args,
            channel_ref: origin.channel_ref.clone(),
            topic: origin.topic.clone(),
            triggerer_master_id: origin.triggerer_master_id.clone(),
            triggerer_display: None, // TurnCtx does not carry display name yet; can be added later
        };
        let decision = broker.request_decision(&req_ctx).await;
        return map_broker_decision(decision, mgr, tool_name, tool_args, ctx, iteration);
    }

    // CLI fallback path: no broker, use CLI prompt.
    let request = ApprovalRequest {
        tool_name: tool_name.to_string(),
        arguments: tool_args.clone(),
    };

    let decision = mgr.prompt_cli(&request);
    mgr.record_decision(
        tool_name,
        tool_args,
        &decision,
        ctx.channel_name,
        cli_reason_for(&decision),
        serde_json::json!({}),
    );

    // Stream status update for Deny/Replace (CLI path).
    if decision == ApprovalResponse::No {
        if let Some(tx) = ctx.on_delta {
            let _ = tx
                .send(StreamDelta::Status(format!(
                    "\u{274c} {}: Denied by user.\n",
                    tool_name
                )))
                .await;
        }
    }
    if let ApprovalResponse::ReplaceWith(_) = &decision {
        if let Some(tx) = ctx.on_delta {
            let _ = tx
                .send(StreamDelta::Status(format!(
                    "\u{270f} {}: replaced by user\n",
                    tool_name
                )))
                .await;
        }
    }

    cli_decision_to_outcome(decision, tool_name)
}
