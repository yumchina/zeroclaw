//! End-to-end coverage for broker fan_out + cancel_approval wiring.
//! Uses lightweight in-process FakeChannel (duplicated minimally from
//! broker::tests since #[cfg(test)] items aren't reachable cross-crate).

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use tempfile::TempDir;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_infra::identity_store::IdentityResolver;
use zeroclaw_runtime::approval::{
    ApprovalBroker, ApprovalGrantStore, BrokerDecision, BrokerRequestCtx,
    ChannelDirectory, Humanizer, SqliteGrantStore,
};

struct FakeChannel {
    name: String,
    respond_with: StdMutex<Option<ChannelApprovalResponse>>,
    delay: Duration,
    cancel_calls: StdMutex<Vec<(String, String, String)>>,
}

impl FakeChannel {
    fn new(name: &str, response: Option<ChannelApprovalResponse>, delay_ms: u64) -> Self {
        Self {
            name: name.into(),
            respond_with: StdMutex::new(response),
            delay: Duration::from_millis(delay_ms),
            cancel_calls: StdMutex::new(Vec::new()),
        }
    }
}

impl zeroclaw_api::attribution::Attributable for FakeChannel {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Cli)
    }
    fn alias(&self) -> &str { &self.name }
}

#[async_trait]
impl Channel for FakeChannel {
    fn name(&self) -> &str { &self.name }
    async fn send(&self, _: &SendMessage) -> anyhow::Result<()> { Ok(()) }
    async fn listen(&self, _: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        Ok(())
    }
    async fn request_approval(
        &self,
        _: &str,
        _: &ChannelApprovalRequest,
    ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
        tokio::time::sleep(self.delay).await;
        Ok(self.respond_with.lock().unwrap().clone())
    }
    async fn cancel_approval(
        &self,
        approval_id: &str,
        recipient: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        self.cancel_calls.lock().unwrap().push((
            approval_id.to_string(),
            recipient.to_string(),
            reason.to_string(),
        ));
        Ok(())
    }
}

struct StaticDirectory(Vec<(String, Arc<dyn Channel>)>);
impl ChannelDirectory for StaticDirectory {
    fn lookup(&self, channel_ref: &str) -> Option<Arc<dyn Channel>> {
        self.0.iter().find(|(k, _)| k == channel_ref).map(|(_, v)| v.clone())
    }
}

struct EmptyIdentity;
impl IdentityResolver for EmptyIdentity {
    fn resolve(&self, _: &str, _: &str, _: bool) -> Option<String> { None }
    fn issue_code(&self, _: &str) -> Option<String> { None }
    fn redeem_code(&self, _: &str, _: &str, _: &str) -> Result<String, String> { Err("n/a".into()) }
    fn unbind(&self, _: &str, _: &str) -> bool { false }
    fn reverse_lookup(&self, _: &str, _: &str) -> Option<String> { None }
}

fn fresh_store() -> (TempDir, Arc<dyn ApprovalGrantStore>) {
    let tmp = TempDir::new().unwrap();
    let s = SqliteGrantStore::new(tmp.path()).unwrap();
    (tmp, Arc::new(s) as Arc<dyn ApprovalGrantStore>)
}

fn broker_with(
    dir: Arc<dyn ChannelDirectory>,
    grants: Arc<dyn ApprovalGrantStore>,
    superusers: Vec<String>,
    master_channel: Option<String>,
    timeout: Duration,
) -> ApprovalBroker {
    let su = Arc::new(superusers);
    let mc = Arc::new(master_channel);
    ApprovalBroker::new(
        grants,
        Arc::new(EmptyIdentity),
        dir,
        Arc::new(Humanizer::new(None, Duration::from_secs(10))),
        Arc::new(move || (*su).clone()),
        Arc::new(move || (*mc).clone()),
        timeout,
    )
}

#[tokio::test]
async fn end_to_end_two_superusers_winner_cancels_loser() {
    let (_t, grants) = fresh_store();
    let fast = Arc::new(FakeChannel::new(
        "dawnim.work",
        Some(ChannelApprovalResponse::Approve),
        50,
    ));
    let dir = Arc::new(StaticDirectory(vec![("dawnim.work".into(), fast.clone())]));
    let b = broker_with(
        dir,
        grants,
        vec!["u_admin1".into(), "u_admin2".into()],
        Some("dawnim.work".into()),
        Duration::from_secs(1),
    );
    let ctx = BrokerRequestCtx {
        tool_name: "shell",
        tool_args: &serde_json::json!({}),
        channel_ref: "dawnim.work".into(),
        topic: None,
        triggerer_master_id: Some("u_alice".into()),
        triggerer_display: None,
    };
    let decision = b.request_decision(&ctx).await;
    assert!(matches!(decision, BrokerDecision::Approve { .. }));
    // One of the two recipients must have been cancelled (the loser).
    let calls = fast.cancel_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "expected exactly one cancel call, got {:?}", calls);
    let (_id, _recipient, reason) = &calls[0];
    assert!(
        reason.contains("approved") || reason.contains("同意"),
        "reason should mention the winning decision: {reason}"
    );
}

#[tokio::test]
async fn end_to_end_all_timeout_cancels_nobody() {
    let (_t, grants) = fresh_store();
    let slow = Arc::new(FakeChannel::new(
        "dawnim.work",
        Some(ChannelApprovalResponse::Approve),
        5_000, // way past broker timeout
    ));
    let dir = Arc::new(StaticDirectory(vec![("dawnim.work".into(), slow.clone())]));
    let b = broker_with(
        dir,
        grants,
        vec!["u_admin1".into(), "u_admin2".into()],
        Some("dawnim.work".into()),
        Duration::from_millis(200),
    );
    let ctx = BrokerRequestCtx {
        tool_name: "shell",
        tool_args: &serde_json::json!({}),
        channel_ref: "dawnim.work".into(),
        topic: None,
        triggerer_master_id: Some("u_alice".into()),
        triggerer_display: None,
    };
    let decision = b.request_decision(&ctx).await;
    assert!(matches!(decision, BrokerDecision::Deny { .. }));
    assert_eq!(slow.cancel_calls.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn end_to_end_self_path_no_fan_out_no_cancel() {
    let (_t, grants) = fresh_store();
    let ch = Arc::new(FakeChannel::new(
        "dawnim.work",
        Some(ChannelApprovalResponse::Approve),
        20,
    ));
    let dir = Arc::new(StaticDirectory(vec![("dawnim.work".into(), ch.clone())]));
    let b = broker_with(
        dir,
        grants,
        vec!["u_admin".into()],
        Some("dawnim.work".into()),
        Duration::from_secs(1),
    );
    let ctx = BrokerRequestCtx {
        tool_name: "shell",
        tool_args: &serde_json::json!({}),
        channel_ref: "dawnim.work".into(),
        topic: None,
        triggerer_master_id: Some("u_admin".into()), // triggerer IS the superuser
        triggerer_display: None,
    };
    let _ = b.request_decision(&ctx).await;
    assert_eq!(
        ch.cancel_calls.lock().unwrap().len(),
        0,
        "self path produces a single target — no cancel should fire"
    );
}
