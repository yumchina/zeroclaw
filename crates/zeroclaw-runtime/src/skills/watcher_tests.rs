//! Integration tests for skill hot reload.

use std::time::Duration;
use tempfile::TempDir;

#[cfg(test)]
mod hot_reload_tests {
    use super::*;

    #[tokio::test]
    async fn test_watcher_detects_new_skill() {
        let temp_dir = TempDir::new().unwrap();
        let skills_dir = temp_dir.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();

        let config = crate::skills::WatcherConfig {
            skills_dir: skills_dir.clone(),
            debounce_ms: 100,
            watch_mode: "poll".to_string(),  // Use poll for testing
            poll_interval_secs: 1,
        };

        let (_handle, mut reload_rx) = crate::skills::spawn_skill_watcher(config)
            .expect("Failed to spawn watcher");

        // Create a new skill directory
        let skill_dir = skills_dir.join("test_skill");
        std::fs::create_dir(&skill_dir).unwrap();

        // Wait for poll to detect change (need 2 poll cycles)
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Check if reload was triggered
        let reload_req = tokio::time::timeout(
            Duration::from_secs(5),
            reload_rx.recv()
        ).await;

        assert!(reload_req.is_ok(), "Should receive reload request");
    }

    #[tokio::test]
    async fn test_registry_manager_concurrent_swap() {
        use crate::tools::{ToolRegistryManager, Tool, ToolResult};
        use async_trait::async_trait;
        use std::sync::Arc;

        struct DummyTool { _n: usize }

        #[async_trait]
        impl Tool for DummyTool {
            fn name(&self) -> &str { "dummy" }
            fn description(&self) -> &str { "dummy tool" }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
                Ok(ToolResult {
                    success: true,
                    output: "dummy".to_string(),
                    error: None,
                })
            }
        }

        let mgr = Arc::new(ToolRegistryManager::new(vec![
            Box::new(DummyTool { _n: 1 }) as Box<dyn Tool>
        ]));

        // Spawn multiple tasks that swap concurrently
        let mut handles = vec![];
        for i in 0..10 {
            let mgr_clone = Arc::clone(&mgr);
            let handle = tokio::spawn(async move {
                let tools = vec![
                    Box::new(DummyTool { _n: i }) as Box<dyn Tool>
                ];
                mgr_clone.atomic_swap(tools)
            });
            handles.push(handle);
        }

        // Wait for all tasks
        for handle in handles {
            handle.await.unwrap();
        }

        // Version should be 11 (initial 1 + 10 swaps)
        assert_eq!(mgr.version(), 11);
    }
}
