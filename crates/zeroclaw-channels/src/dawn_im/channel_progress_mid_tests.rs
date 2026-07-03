use super::*;

fn channel_with_test_state() -> DawnIMChannel {
    let cfg = zeroclaw_config::schema::DawnIMConfig {
        enabled: true,
        ws_url: "ws://localhost:5200".into(),
        uid: "bot_uid_1".into(),
        token: String::new(),
        device_id: "test-device".into(),
        allowed_users: vec!["*".into()],
        ..Default::default()
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let memory: Arc<dyn zeroclaw_api::memory_traits::Memory> = Arc::new(
        zeroclaw_memory::SqliteMemory::new_named("sqlite", tmp.path(), "progress_mid_test")
            .expect("memory"),
    );
    DawnIMChannel::from_config(&cfg, "test", tmp.path(), memory)
}

#[tokio::test]
async fn agent_end_reuses_agent_start_mid_when_provided_id_mismatches() {
    let channel = channel_with_test_state();
    let recipient = "user--alice";

    let start_mid = channel
        .resolve_progress_mid(
            recipient,
            "incoming-msg-1",
            &zeroclaw_api::channel::ProgressPhase::AgentStart {
                provider: "p".into(),
                model: "m".into(),
            },
        )
        .await;
    assert_eq!(start_mid, "incoming-msg-1");

    let end_mid = channel
        .resolve_progress_mid(
            recipient,
            "incoming-msg-2",
            &zeroclaw_api::channel::ProgressPhase::AgentEnd,
        )
        .await;
    assert_eq!(end_mid, start_mid);
}

#[tokio::test]
async fn provided_mid_is_persisted_for_non_start_phases_when_no_existing_mid() {
    let channel = channel_with_test_state();
    let recipient = "user--bob";

    let end_mid = channel
        .resolve_progress_mid(
            recipient,
            "incoming-msg-9",
            &zeroclaw_api::channel::ProgressPhase::AgentEnd,
        )
        .await;
    assert_eq!(end_mid, "incoming-msg-9");

    let tool_mid = channel
        .resolve_progress_mid(
            recipient,
            "",
            &zeroclaw_api::channel::ProgressPhase::ToolStart {
                tool: "shell".into(),
                tool_call_id: Some("call-1".into()),
            },
        )
        .await;
    assert_eq!(tool_mid, "incoming-msg-9");
}

#[tokio::test]
async fn generated_agent_start_mid_is_reused_by_agent_end() {
    let channel = channel_with_test_state();
    let recipient = "user--charlie";

    let start_mid = channel
        .resolve_progress_mid(
            recipient,
            "",
            &zeroclaw_api::channel::ProgressPhase::AgentStart {
                provider: "p".into(),
                model: "m".into(),
            },
        )
        .await;
    assert!(start_mid.starts_with("zc-progress-"));

    let end_mid = channel
        .resolve_progress_mid(
            recipient,
            "",
            &zeroclaw_api::channel::ProgressPhase::AgentEnd,
        )
        .await;
    assert_eq!(end_mid, start_mid);
}
