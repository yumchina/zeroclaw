// src/approval/mod.rs
pub mod card;

pub use card::{WkApprovalAction, WkApprovalCard, build_approval_card, build_intervention_card};

use std::collections::HashMap;
use tokio::sync::RwLock;
use zeroclaw_api::channel::{ChannelApprovalResponse, ChannelInterventionResponse};

/// Struct enclosing oneshot channel and its associated recipient.
pub struct ActivePendingApproval {
    pub tx: tokio::sync::oneshot::Sender<ChannelApprovalResponse>,
    pub recipient: String,
}

/// Type alias for the pending approvals map.
/// Key = approval_id, Value = ActivePendingApproval enclosing the sender.
pub type PendingApprovals = RwLock<HashMap<String, ActivePendingApproval>>;

/// Type alias for the pending interventions map.
/// Key = approval_id, Value = oneshot sender to resolve the intervention.
pub type PendingInterventions =
    RwLock<HashMap<String, tokio::sync::oneshot::Sender<ChannelInterventionResponse>>>;
