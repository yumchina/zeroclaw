//! ApprovalBroker — coordinates per-tool-call approval decisions.

use crate::approval::decision_reason::*;
use crate::approval::grant_store::{ApprovalGrant, ApprovalGrantStore};
use crate::approval::humanize::Humanizer;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelApprovalRequest, ChannelApprovalResponse};
use zeroclaw_infra::identity_store::IdentityResolver;

#[derive(Debug, Clone)]
pub struct BrokerRequestCtx<'a> {
    pub tool_name: &'a str,
    pub tool_args: &'a serde_json::Value,
    pub channel_ref: String,
    pub topic: Option<String>,
    pub triggerer_master_id: Option<String>,
    pub triggerer_display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerDecision {
    Approve {
        reason: &'static str,
        grant_id: Option<String>,
    },
    Deny {
        reason: &'static str,
    },
    Replace {
        replacement: String,
        reason: &'static str,
    },
}

pub trait ChannelDirectory: Send + Sync {
    fn lookup(&self, channel_ref: &str) -> Option<Arc<dyn Channel>>;
}

fn compute_cancel_reason(response: &ChannelApprovalResponse) -> String {
    use crate::i18n;
    let decision_key = match response {
        ChannelApprovalResponse::Approve => "event-approval-decision-approve",
        ChannelApprovalResponse::AlwaysApprove => "event-approval-decision-always",
        ChannelApprovalResponse::Deny | ChannelApprovalResponse::DenyWithEdit { .. } => {
            "event-approval-decision-deny"
        }
    };
    let decision = i18n::get_event_string(decision_key).unwrap_or_default();
    i18n::get_event_string_with_args(
        "event-approval-cancelled-status",
        &[("decision", decision.as_str())],
    )
}

pub struct ApprovalBroker {
    pub(crate) grants: Arc<dyn ApprovalGrantStore>,
    pub(crate) identity: Arc<dyn IdentityResolver>,
    pub(crate) directory: Arc<dyn ChannelDirectory>,
    pub(crate) humanizer: Arc<Humanizer>,
    pub(crate) superusers_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub(crate) master_channel_resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub(crate) approval_timeout: std::time::Duration,
}

impl ApprovalBroker {
    pub fn new(
        grants: Arc<dyn ApprovalGrantStore>,
        identity: Arc<dyn IdentityResolver>,
        directory: Arc<dyn ChannelDirectory>,
        humanizer: Arc<Humanizer>,
        superusers_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
        master_channel_resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
        approval_timeout: std::time::Duration,
    ) -> Self {
        Self {
            grants,
            identity,
            directory,
            humanizer,
            superusers_resolver,
            master_channel_resolver,
            approval_timeout,
        }
    }

    pub async fn request_decision(&self, ctx: &BrokerRequestCtx<'_>) -> BrokerDecision {
        // 1) Cached grant?
        let cached = self.grants.get(
            &ctx.channel_ref,
            ctx.topic.as_deref(),
            ctx.triggerer_master_id.as_deref().unwrap_or(""),
            ctx.tool_name,
        );
        if let Ok(Some(g)) = cached {
            return BrokerDecision::Approve {
                reason: CACHED_GRANT,
                grant_id: Some(g.id),
            };
        }

        // 2) Empty superuser list -> deny
        let superusers = (self.superusers_resolver)();
        if superusers.is_empty() {
            return BrokerDecision::Deny {
                reason: NO_SUPERUSER_CONFIGURED,
            };
        }
        let master_channel = match (self.master_channel_resolver)() {
            Some(m) => m,
            None => {
                return BrokerDecision::Deny {
                    reason: NO_MASTER_CHANNEL,
                };
            }
        };

        // 3) Self vs proxy
        let is_self = ctx
            .triggerer_master_id
            .as_ref()
            .map(|t| superusers.iter().any(|s| s == t))
            .unwrap_or(false);

        // 4) Resolve targets
        let targets = if is_self {
            vec![(
                ctx.channel_ref.clone(),
                ctx.triggerer_master_id.clone().unwrap_or_default(),
            )]
        } else {
            self.resolve_proxy_targets(&superusers, &ctx.channel_ref, &master_channel)
        };

        // 5) Humanize once, shared across targets
        let card_summary = self
            .humanizer
            .humanize(
                ctx.tool_name,
                ctx.tool_args,
                ctx.triggerer_display.as_deref(),
                ctx.topic.as_deref(),
                &ctx.channel_ref,
            )
            .await;

        // 6) Fan out
        let approval_id = uuid::Uuid::new_v4().to_string();
        let request = ChannelApprovalRequest {
            tool_name: ctx.tool_name.to_string(),
            arguments_summary: card_summary,
            raw_arguments: None,
            thread_ts: ctx.topic.clone(),
            approval_id: Some(approval_id.clone()),
        };
        let (winner, winning_target) = self.fan_out(&targets, &approval_id, &request).await;

        match winner {
            None => BrokerDecision::Deny {
                reason: ALL_SUPERUSERS_TIMEOUT,
            },
            Some(ChannelApprovalResponse::Approve) => BrokerDecision::Approve {
                reason: INTERACTIVE_APPROVE,
                grant_id: None,
            },
            Some(ChannelApprovalResponse::AlwaysApprove) => {
                let grant = ApprovalGrant::new(
                    ctx.channel_ref.clone(),
                    ctx.topic.clone(),
                    ctx.triggerer_master_id.clone().unwrap_or_default(),
                    ctx.tool_name.to_string(),
                    self.identify_decider(
                        &winning_target.as_ref().map(|(c, _)| c.clone()),
                        &superusers,
                        ctx,
                    ),
                    winning_target
                        .as_ref()
                        .map(|(c, _)| c.clone())
                        .unwrap_or_else(|| ctx.channel_ref.clone()),
                );
                let grant_id = grant.id.clone();
                let _ = self.grants.put(grant);
                BrokerDecision::Approve {
                    reason: INTERACTIVE_ALWAYS,
                    grant_id: Some(grant_id),
                }
            }
            Some(ChannelApprovalResponse::Deny) => BrokerDecision::Deny {
                reason: INTERACTIVE_DENY,
            },
            Some(ChannelApprovalResponse::DenyWithEdit { replacement }) => {
                BrokerDecision::Replace {
                    replacement,
                    reason: INTERACTIVE_REPLACE,
                }
            }
        }
    }

    fn resolve_proxy_targets(
        &self,
        superusers: &[String],
        triggering_channel: &str,
        master_channel: &str,
    ) -> Vec<(String, String)> {
        superusers
            .iter()
            .map(|su| {
                if let Some(uid) = self.identity.reverse_lookup(su, triggering_channel) {
                    (triggering_channel.to_string(), uid)
                } else {
                    (master_channel.to_string(), su.clone())
                }
            })
            .collect()
    }

    async fn fan_out(
        &self,
        targets: &[(String, String)],
        approval_id: &str,
        request: &ChannelApprovalRequest,
    ) -> (
        Option<ChannelApprovalResponse>,
        Option<(String, String)>,
    ) {
        use tokio::task::JoinSet;
        let mut set: JoinSet<(String, String, anyhow::Result<Option<ChannelApprovalResponse>>)> =
            JoinSet::new();
        let mut alive_targets: Vec<(String, String, Arc<dyn Channel>)> = Vec::new();
        for (channel_ref, recipient) in targets {
            if let Some(ch) = self.directory.lookup(channel_ref) {
                let ch_clone = ch.clone();
                let chref = channel_ref.clone();
                let recipient_clone = recipient.clone();
                let req_clone = request.clone();
                set.spawn(async move {
                    let res = ch_clone
                        .request_approval(&recipient_clone, &req_clone)
                        .await;
                    (chref, recipient_clone, res)
                });
                alive_targets.push((channel_ref.clone(), recipient.clone(), ch));
            }
        }
        let mut winner: Option<(ChannelApprovalResponse, String, String)> = None;
        let timeout = self.approval_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                Some(joined) = set.join_next() => {
                    if let Ok((chref, recipient, Ok(Some(resp)))) = joined {
                        winner = Some((resp, chref, recipient));
                        break;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => { break; }
                else => { break; }
            }
        }
        if let Some((ref resp, ref winning_chref, ref winning_recipient)) = winner {
            let reason = compute_cancel_reason(resp);
            for (chref, recipient, ch) in alive_targets.iter() {
                if (chref.as_str(), recipient.as_str())
                    != (winning_chref.as_str(), winning_recipient.as_str())
                {
                    if let Err(e) = ch
                        .cancel_approval(approval_id, recipient, &reason)
                        .await
                    {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                                .with_attrs(::serde_json::json!({
                                    "approval_id": approval_id,
                                    "channel_ref": chref,
                                    "recipient": recipient,
                                    "error": format!("{e}"),
                                })),
                            "broker: cancel_approval failed (best-effort)"
                        );
                    }
                }
            }
        }
        set.shutdown().await;
        match winner {
            Some((r, c, rec)) => (Some(r), Some((c, rec))),
            None => (None, None),
        }
    }

    fn identify_decider(
        &self,
        winning_channel: &Option<String>,
        superusers: &[String],
        ctx: &BrokerRequestCtx<'_>,
    ) -> String {
        // Best-effort attribution; if we can't pin a specific superuser,
        // fall back to the triggerer (self-approval path).
        if let (Some(_chref), Some(triggerer)) = (winning_channel, ctx.triggerer_master_id.as_ref())
        {
            if superusers.iter().any(|s| s == triggerer) {
                return triggerer.clone();
            }
        }
        superusers.first().cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::grant_store::SqliteGrantStore;
    use crate::approval::humanize::Humanizer;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tempfile::TempDir;
    use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

    // ── Fake channel ────────────────────────────────────────────────
    struct FakeChannel {
        name: String,
        respond_with: StdMutex<Option<ChannelApprovalResponse>>,
        delay: Duration,
        cancel_calls: StdMutex<Vec<(String, String, String)>>,
        cancel_should_fail: StdMutex<bool>,
    }
    impl FakeChannel {
        fn new(name: &str, response: Option<ChannelApprovalResponse>) -> Self {
            Self {
                name: name.into(),
                respond_with: StdMutex::new(response),
                delay: Duration::from_millis(10),
                cancel_calls: StdMutex::new(Vec::new()),
                cancel_should_fail: StdMutex::new(false),
            }
        }
    }
    impl zeroclaw_api::attribution::Attributable for FakeChannel {
        fn role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Channel(zeroclaw_api::attribution::ChannelKind::Cli)
        }
        fn alias(&self) -> &str {
            &self.name
        }
    }

    #[async_trait::async_trait]
    impl Channel for FakeChannel {
        fn name(&self) -> &str {
            &self.name
        }
        async fn send(&self, _: &SendMessage) -> anyhow::Result<()> {
            Ok(())
        }
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
            if *self.cancel_should_fail.lock().unwrap() {
                anyhow::bail!("fake channel cancel failure (test injection)");
            }
            Ok(())
        }
    }

    struct StaticDirectory {
        entries: Vec<(String, Arc<dyn Channel>)>,
    }
    impl ChannelDirectory for StaticDirectory {
        fn lookup(&self, channel_ref: &str) -> Option<Arc<dyn Channel>> {
            self.entries
                .iter()
                .find(|(k, _)| k == channel_ref)
                .map(|(_, v)| v.clone())
        }
    }

    // ── Fake identity resolver ──────────────────────────────────────
    struct FakeIdentity(StdMutex<std::collections::HashMap<(String, String), String>>);
    impl FakeIdentity {
        fn empty() -> Self {
            Self(StdMutex::new(Default::default()))
        }
        fn bind(&self, master_id: &str, channel_ref: &str, uid: &str) {
            self.0
                .lock()
                .unwrap()
                .insert((master_id.into(), channel_ref.into()), uid.into());
        }
    }
    impl IdentityResolver for FakeIdentity {
        fn resolve(&self, _: &str, _: &str, _: bool) -> Option<String> {
            None
        }
        fn issue_code(&self, _: &str) -> Option<String> {
            None
        }
        fn redeem_code(&self, _: &str, _: &str, _: &str) -> Result<String, String> {
            Err("n/a".into())
        }
        fn unbind(&self, _: &str, _: &str) -> bool {
            false
        }
        fn reverse_lookup(&self, master_id: &str, channel_ref: &str) -> Option<String> {
            self.0
                .lock()
                .unwrap()
                .get(&(master_id.into(), channel_ref.into()))
                .cloned()
        }
    }

    fn broker(
        directory: Arc<dyn ChannelDirectory>,
        identity: Arc<dyn IdentityResolver>,
        grants: Arc<dyn ApprovalGrantStore>,
        superusers: Vec<String>,
        master_channel: Option<String>,
    ) -> ApprovalBroker {
        let su = Arc::new(superusers);
        let mc = Arc::new(master_channel);
        ApprovalBroker {
            grants,
            identity,
            directory,
            humanizer: Arc::new(Humanizer::new(None, Duration::from_secs(10))),
            superusers_resolver: Arc::new(move || (*su).clone()),
            master_channel_resolver: Arc::new(move || (*mc).clone()),
            approval_timeout: Duration::from_millis(500),
        }
    }

    fn fresh_store() -> (TempDir, Arc<dyn ApprovalGrantStore>) {
        let tmp = TempDir::new().unwrap();
        let s = SqliteGrantStore::new(tmp.path()).unwrap();
        (tmp, Arc::new(s) as Arc<dyn ApprovalGrantStore>)
    }

    #[tokio::test]
    async fn deny_when_no_superusers() {
        let (_t, grants) = fresh_store();
        let dir = Arc::new(StaticDirectory { entries: vec![] });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(dir, id, grants, vec![], Some("dawnim.work".into()));
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        assert_eq!(
            b.request_decision(&ctx).await,
            BrokerDecision::Deny {
                reason: NO_SUPERUSER_CONFIGURED
            }
        );
    }

    #[tokio::test]
    async fn deny_when_no_master_channel() {
        let (_t, grants) = fresh_store();
        let dir = Arc::new(StaticDirectory { entries: vec![] });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(dir, id, grants, vec!["u_admin".into()], None);
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        assert_eq!(
            b.request_decision(&ctx).await,
            BrokerDecision::Deny {
                reason: NO_MASTER_CHANNEL
            }
        );
    }

    #[tokio::test]
    async fn cached_grant_short_circuits() {
        let (_t, grants) = fresh_store();
        grants
            .put(ApprovalGrant::new(
                "dawnim.work".into(),
                Some("db_lock".into()),
                "u_alice".into(),
                "shell".into(),
                "u_admin".into(),
                "dawnim.work".into(),
            ))
            .unwrap();
        let dir = Arc::new(StaticDirectory { entries: vec![] });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: Some("db_lock".into()),
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        match b.request_decision(&ctx).await {
            BrokerDecision::Approve { reason, grant_id } => {
                assert_eq!(reason, CACHED_GRANT);
                assert!(grant_id.is_some());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn self_path_when_triggerer_is_superuser() {
        let (_t, grants) = fresh_store();
        let fake = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), fake.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_admin".into()), // is superuser
            triggerer_display: None,
        };
        assert!(matches!(
            b.request_decision(&ctx).await,
            BrokerDecision::Approve {
                reason: INTERACTIVE_APPROVE,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn proxy_path_fans_out_to_all_superusers() {
        let (_t, grants) = fresh_store();
        let a = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        let b_ch = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Deny),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), a.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let _ = b_ch;
        let broker = broker(
            dir,
            id,
            grants,
            vec!["u_admin1".into(), "u_admin2".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()), // not superuser
            triggerer_display: Some("Alice".into()),
        };
        // Both targets resolve to the same fake channel (master_channel fallback);
        // FakeChannel returns Approve — broker accepts.
        assert!(matches!(
            broker.request_decision(&ctx).await,
            BrokerDecision::Approve {
                reason: INTERACTIVE_APPROVE,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn always_approve_writes_grant() {
        let (_t, grants) = fresh_store();
        let fake = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::AlwaysApprove),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), fake.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants.clone(),
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: Some("t1".into()),
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        match b.request_decision(&ctx).await {
            BrokerDecision::Approve {
                reason: INTERACTIVE_ALWAYS,
                grant_id: Some(_),
            } => {
                let stored = grants
                    .get("dawnim.work", Some("t1"), "u_alice", "shell")
                    .unwrap();
                assert!(stored.is_some());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn approve_does_not_write_grant() {
        let (_t, grants) = fresh_store();
        let fake = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), fake)],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants.clone(),
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: Some("t1".into()),
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        let _ = b.request_decision(&ctx).await;
        assert!(
            grants
                .get("dawnim.work", Some("t1"), "u_alice", "shell")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn all_timeout_returns_deny_with_timeout_reason() {
        let (_t, grants) = fresh_store();
        let slow = Arc::new(FakeChannel {
            name: "dawnim.work".into(),
            respond_with: StdMutex::new(Some(ChannelApprovalResponse::Approve)),
            delay: Duration::from_secs(5),
            cancel_calls: StdMutex::new(Vec::new()),
            cancel_should_fail: StdMutex::new(false),
        });
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), slow)],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        // broker.approval_timeout = 500ms; slow channel takes 5s → timeout
        assert_eq!(
            b.request_decision(&ctx).await,
            BrokerDecision::Deny {
                reason: ALL_SUPERUSERS_TIMEOUT
            }
        );
    }

    #[tokio::test]
    async fn fan_out_cancel_reason_carries_decision() {
        let (_t, grants) = fresh_store();
        let fast = Arc::new(FakeChannel {
            name: "dawnim.work".into(),
            respond_with: StdMutex::new(Some(ChannelApprovalResponse::AlwaysApprove)),
            delay: Duration::from_millis(5),
            cancel_calls: StdMutex::new(Vec::new()),
            cancel_should_fail: StdMutex::new(false),
        });
        let slow = Arc::new(FakeChannel {
            name: "dawnim.work".into(),
            respond_with: StdMutex::new(Some(ChannelApprovalResponse::Deny)),
            delay: Duration::from_millis(500),
            cancel_calls: StdMutex::new(Vec::new()),
            cancel_should_fail: StdMutex::new(false),
        });
        // Both targets resolve to the same channel_ref to force fan-out across
        // two recipients; using two distinct FakeChannel instances would
        // require multi-entry StaticDirectory rework. We rely on broker
        // calling cancel_approval on the same Arc for the loser recipient.
        let _ = slow;
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), fast.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin1".into(), "u_admin2".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        let _ = b.request_decision(&ctx).await;
        // The reason string contains the localized "always approved" decision.
        // We assert on the substring without depending on global LOCALE state
        // (defaults to "en" in test process).
        let calls = fast.cancel_calls.lock().unwrap().clone();
        assert!(!calls.is_empty(), "expected at least one cancel call");
        let (_id, _recipient, reason) = &calls[0];
        assert!(
            reason.contains("always approved") || reason.contains("始终允许"),
            "reason should mention always-approve: {reason}"
        );
    }

    #[tokio::test]
    async fn fan_out_propagates_broker_approval_id() {
        // FakeChannel that captures the most recent request it was asked to handle.
        struct CapturingChannel {
            inner: Arc<FakeChannel>,
            last_request: Arc<StdMutex<Option<ChannelApprovalRequest>>>,
        }
        impl zeroclaw_api::attribution::Attributable for CapturingChannel {
            fn role(&self) -> zeroclaw_api::attribution::Role {
                self.inner.role()
            }
            fn alias(&self) -> &str { self.inner.alias() }
        }
        #[async_trait::async_trait]
        impl Channel for CapturingChannel {
            fn name(&self) -> &str { self.inner.name() }
            async fn send(&self, m: &SendMessage) -> anyhow::Result<()> { self.inner.send(m).await }
            async fn listen(&self, t: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
                self.inner.listen(t).await
            }
            async fn request_approval(
                &self,
                r: &str,
                req: &ChannelApprovalRequest,
            ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
                *self.last_request.lock().unwrap() = Some(req.clone());
                self.inner.request_approval(r, req).await
            }
            async fn cancel_approval(
                &self,
                a: &str,
                r: &str,
                reason: &str,
            ) -> anyhow::Result<()> {
                self.inner.cancel_approval(a, r, reason).await
            }
        }

        let (_t, grants) = fresh_store();
        let inner = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        let last_request = Arc::new(StdMutex::new(None));
        let capture: Arc<dyn Channel> = Arc::new(CapturingChannel {
            inner: inner.clone(),
            last_request: last_request.clone(),
        });
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), capture)],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        let _ = b.request_decision(&ctx).await;
        let captured = last_request.lock().unwrap().clone()
            .expect("FakeChannel should have received a request");
        let id = captured.approval_id.expect("request must carry broker approval_id");
        assert!(!id.is_empty(), "approval_id must be non-empty");
        assert_eq!(id.len(), 36, "uuid v4 should be 36 chars, got: {id}");
    }

    #[tokio::test]
    async fn fan_out_cancels_losers_by_chref_and_recipient() {
        // FakeIdentity that maps u_admin1 → "u_admin1_local" on dawnim.work
        // and leaves u_admin2 to fall back via master_channel.
        let (_t, grants) = fresh_store();
        let id = Arc::new(FakeIdentity::empty());
        id.bind("u_admin1", "dawnim.work", "u_admin1_local");
        // u_admin2 not bound → master_channel fallback.

        let ch = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), ch.clone())],
        });
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin1".into(), "u_admin2".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        let _ = b.request_decision(&ctx).await;
        let calls = ch.cancel_calls.lock().unwrap().clone();
        // Exactly one of the two targets should have been cancelled
        // (the FakeChannel returns Approve immediately for whichever request
        // hits first, so the other one is the loser).
        assert_eq!(calls.len(), 1, "expected one cancel call, got {:?}", calls);
        let (_id, recipient, _reason) = &calls[0];
        assert!(
            recipient == "u_admin1_local" || recipient == "u_admin2",
            "loser recipient should be one of the resolved targets, got: {recipient}"
        );
    }

    #[tokio::test]
    async fn fan_out_cancel_failure_does_not_block() {
        let (_t, grants) = fresh_store();
        let ch = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        *ch.cancel_should_fail.lock().unwrap() = true;
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), ch.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin1".into(), "u_admin2".into()],
            Some("dawnim.work".into()),
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
        assert!(matches!(
            decision,
            BrokerDecision::Approve {
                reason: INTERACTIVE_APPROVE,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn winner_self_not_cancelled() {
        let (_t, grants) = fresh_store();
        let ch = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), ch.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        let _ = b.request_decision(&ctx).await;
        assert_eq!(
            ch.cancel_calls.lock().unwrap().len(),
            0,
            "single-target fan-out should never cancel"
        );
    }
}
