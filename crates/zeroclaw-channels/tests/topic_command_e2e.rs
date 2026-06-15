//! End-to-end integration tests for the /topic cross-channel binding feature.
//!
//! These tests exercise the cross-crate seams: TopicBindingRegistry
//! persistence (file round-trip across reload) and SqliteMemory
//! list_unified_topics scoping by master_id. Pure-function unit tests
//! live in their respective crates' inline test modules; this file
//! covers what unit tests by definition cannot.

use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn feishu_bound_to_topic_routes_to_unified_master_topic_key() {
    let tmp = TempDir::new().unwrap();
    let reg = zeroclaw_infra::make_topic_binding_registry(tmp.path()).unwrap();
    reg.set("feishu.work", "ou_alice", "db_lock");

    // Reload to simulate restart — binding must persist.
    drop(reg);
    let reg = zeroclaw_infra::make_topic_binding_registry(tmp.path()).unwrap();
    assert_eq!(
        reg.get("feishu.work", "ou_alice"),
        Some("db_lock".to_string()),
        "binding must survive registry reload"
    );

    // sanity: an unrelated key is still None.
    assert_eq!(reg.get("feishu.work", "ou_bob"), None);
    // Keep Arc reachable through end of test.
    let _ = Arc::clone(&reg);
}

#[test]
fn topic_binding_is_per_channel_sender_pair() {
    let tmp = TempDir::new().unwrap();
    let reg = zeroclaw_infra::make_topic_binding_registry(tmp.path()).unwrap();
    reg.set("feishu.work", "ou_alice", "topic_a");
    reg.set("feishu.work", "ou_bob", "topic_b");
    reg.set("wecom_ws.default", "ou_alice", "topic_c");

    assert_eq!(reg.get("feishu.work", "ou_alice"), Some("topic_a".into()));
    assert_eq!(reg.get("feishu.work", "ou_bob"), Some("topic_b".into()));
    assert_eq!(
        reg.get("wecom_ws.default", "ou_alice"),
        Some("topic_c".into())
    );
}

#[tokio::test]
async fn topic_listing_returns_only_master_ids_topics() {
    use zeroclaw_memory::SqliteMemory;
    use zeroclaw_memory::traits::{Memory, MemoryCategory};
    let tmp = TempDir::new().unwrap();
    let mem = SqliteMemory::new_named("test", tmp.path(), "dawn_im_work").unwrap();

    // Seed: u_alice has two topics, u_bob has one. Each session_id needs a
    // unique storage key because (agent_id, key) is UNIQUE.
    for (i, sid) in [
        "unified_u_alice_db_lock",
        "unified_u_alice_migrations",
        "unified_u_bob_secret",
    ]
    .iter()
    .enumerate()
    {
        mem.store(
            &format!("k_{i}"),
            "content",
            MemoryCategory::Core,
            Some(sid),
        )
        .await
        .unwrap();
    }

    let mut alice = mem.list_unified_topics("u_alice").unwrap();
    alice.sort();
    assert_eq!(alice, vec!["db_lock", "migrations"]);

    let bob = mem.list_unified_topics("u_bob").unwrap();
    assert_eq!(bob, vec!["secret"]);

    // No leak across master_ids.
    let nobody = mem.list_unified_topics("u_nobody").unwrap();
    assert!(nobody.is_empty());
}
