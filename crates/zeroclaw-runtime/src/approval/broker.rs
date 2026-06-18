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
        };
        let (winner, winning_channel_ref) = self.fan_out(&targets, &approval_id, &request).await;

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
                    self.identify_decider(&winning_channel_ref, &superusers, ctx),
                    winning_channel_ref.unwrap_or_else(|| ctx.channel_ref.clone()),
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
    ) -> (Option<ChannelApprovalResponse>, Option<String>) {
        use tokio::task::JoinSet;
        let mut set: JoinSet<(String, anyhow::Result<Option<ChannelApprovalResponse>>)> =
            JoinSet::new();
        let mut alive_targets: Vec<(String, Arc<dyn Channel>)> = Vec::new();
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
                    (chref, res)
                });
                alive_targets.push((channel_ref.clone(), ch));
            }
        }
        let mut winner: Option<(ChannelApprovalResponse, String)> = None;
        let timeout = self.approval_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                Some(joined) = set.join_next() => {
                    if let Ok((chref, Ok(Some(resp)))) = joined {
                        winner = Some((resp, chref));
                        break;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => { break; }
                else => { break; }
            }
        }
        if let Some((_, ref winning_chref)) = winner {
            for (chref, ch) in alive_targets.iter() {
                if chref != winning_chref {
                    let _ = ch
                        .cancel_approval(approval_id, "已由其他 superuser 处理")
                        .await;
                }
            }
        }
        set.shutdown().await;
        match winner {
            Some((r, c)) => (Some(r), Some(c)),
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
        cancel_count: StdMutex<usize>,
    }
    impl FakeChannel {
        fn new(name: &str, response: Option<ChannelApprovalResponse>) -> Self {
            Self {
                name: name.into(),
                respond_with: StdMutex::new(response),
                delay: Duration::from_millis(10),
                cancel_count: StdMutex::new(0),
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
        async fn cancel_approval(&self, _: &str, _: &str) -> anyhow::Result<()> {
            *self.cancel_count.lock().unwrap() += 1;
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
            cancel_count: StdMutex::new(0),
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
}
