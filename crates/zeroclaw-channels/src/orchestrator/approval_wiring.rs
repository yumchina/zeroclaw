//! Approval wiring helpers — construct ApprovalManager with grant store + broker.

use std::sync::Arc;
use std::time::Duration;
use zeroclaw_config::schema::{ApprovalConfig, RiskProfileConfig};
use zeroclaw_infra::identity_store::IdentityResolver;
use zeroclaw_runtime::approval::{
    ApprovalBroker, ApprovalGrantStore, ApprovalManager, ChannelDirectory, Humanizer,
};

pub fn build_approval_manager_for_non_interactive(
    risk_profile: &RiskProfileConfig,
    approval_cfg: &ApprovalConfig,
    grants: Option<Arc<dyn ApprovalGrantStore>>,
    identity: Option<Arc<dyn IdentityResolver>>,
    channel_directory: Option<Arc<dyn ChannelDirectory>>,
    superusers_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    master_channel_resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    approval_timeout: Duration,
) -> ApprovalManager {
    let mut mgr = ApprovalManager::for_non_interactive(risk_profile);
    if let Some(g) = grants.clone() {
        mgr = mgr.with_grant_store(g);
    }
    if let (Some(g), Some(id), Some(dir)) = (grants, identity, channel_directory) {
        // Phase 1: no LLM summary_provider wiring — use deterministic fallback.
        // A follow-up PR will resolve approval_cfg.summary_provider to a provider Arc.
        let humanizer = Arc::new(Humanizer::new(
            None,
            Duration::from_secs(approval_cfg.humanize_timeout_secs),
        ));
        let broker = Arc::new(ApprovalBroker::new(
            g,
            id,
            dir,
            humanizer,
            superusers_resolver,
            master_channel_resolver,
            approval_timeout,
        ));
        mgr = mgr.with_broker(broker);
    }
    mgr
}
