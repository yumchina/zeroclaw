//! Integration tests for skill hot reload.

use std::time::Duration;
use tempfile::TempDir;

fn poll_watcher_config(skills_dir: std::path::PathBuf, interval_secs: u64) -> crate::skills::WatcherConfig {
    crate::skills::WatcherConfig {
        skills_dir,
        debounce_ms: 100,
        watch_mode: "poll".to_string(),
        poll_interval_secs: interval_secs,
    }
}

/// Convenience: drain at most one reload request within a timeout.
async fn try_drain_reload(
    reload_rx: &mut tokio::sync::mpsc::Receiver<crate::skills::ReloadRequest>,
    timeout: Duration,
) -> Option<crate::skills::ReloadRequest> {
    tokio::time::timeout(timeout, reload_rx.recv())
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod hot_reload_tests {
    use super::*;

    #[tokio::test]
    async fn test_watcher_detects_new_skill() {
        let temp_dir = TempDir::new().unwrap();
        let skills_dir = temp_dir.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();

        let config = poll_watcher_config(skills_dir.clone(), 1);

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

    /// Multiple rapid changes should produce at most one reload per poll cycle.
    #[tokio::test]
    async fn test_watcher_debounce_multiple_rapid_changes() {
        let temp_dir = TempDir::new().unwrap();
        let skills_dir = temp_dir.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();

        // Longer poll interval so all rapid changes land in one cycle
        let config = poll_watcher_config(skills_dir.clone(), 3);

        let (_handle, mut reload_rx) = crate::skills::spawn_skill_watcher(config)
            .expect("Failed to spawn watcher");

        // Create 3 skill directories in rapid succession
        for i in 0..3 {
            let dir = skills_dir.join(format!("skill_{}", i));
            std::fs::create_dir(&dir).unwrap();
        }

        // Wait for one poll cycle
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Should get only one reload (all 3 dirs created within one poll window)
        let first = try_drain_reload(&mut reload_rx, Duration::from_secs(2)).await;
        assert!(first.is_some(), "Should receive at least one reload after batch creation");

        // No second reload should arrive immediately
        let second = try_drain_reload(&mut reload_rx, Duration::from_millis(500)).await;
        assert!(second.is_none(), "Should NOT receive a second reload within the same poll cycle");
    }

    /// Modifying a skill directory should trigger another reload.
    #[tokio::test]
    async fn test_hot_reload_detects_modified_skill() {
        let temp_dir = TempDir::new().unwrap();
        let skills_dir = temp_dir.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();

        // Pre-create one skill dir
        let skill_dir = skills_dir.join("my_skill");
        std::fs::create_dir(&skill_dir).unwrap();

        let config = poll_watcher_config(skills_dir.clone(), 1);

        let (_handle, mut reload_rx) = crate::skills::spawn_skill_watcher(config)
            .expect("Failed to spawn watcher");

        // Drain initial detection of the pre-created directory
        let _initial = try_drain_reload(&mut reload_rx, Duration::from_secs(4)).await;

        // Modify the skill directory (write a new file to change mtime)
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: my_skill\ndescription: updated\n---\n").unwrap();

        // Wait for poll to detect the modification
        tokio::time::sleep(Duration::from_secs(3)).await;

        let reload_req = try_drain_reload(&mut reload_rx, Duration::from_secs(3)).await;
        assert!(reload_req.is_some(), "Should receive reload after modifying skill directory");
    }

    /// Deleting a skill directory should trigger a reload.
    #[tokio::test]
    async fn test_hot_reload_detects_deleted_skill() {
        let temp_dir = TempDir::new().unwrap();
        let skills_dir = temp_dir.path().join("skills");
        std::fs::create_dir(&skills_dir).unwrap();

        // Pre-create one skill dir
        let skill_dir = skills_dir.join("to_delete");
        std::fs::create_dir(&skill_dir).unwrap();

        let config = poll_watcher_config(skills_dir.clone(), 1);

        let (_handle, mut reload_rx) = crate::skills::spawn_skill_watcher(config)
            .expect("Failed to spawn watcher");

        // Drain initial detection
        let _initial = try_drain_reload(&mut reload_rx, Duration::from_secs(4)).await;

        // Delete the skill directory
        std::fs::remove_dir_all(&skill_dir).unwrap();

        // Wait for poll
        tokio::time::sleep(Duration::from_secs(3)).await;

        let reload_req = try_drain_reload(&mut reload_rx, Duration::from_secs(3)).await;
        assert!(reload_req.is_some(), "Should receive reload after deleting skill directory");
    }

    /// Concurrent changes should not corrupt watcher state or version tracking.
    #[tokio::test]
    async fn test_hot_reload_concurrent_changes_and_swaps() {
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
            Box::new(DummyTool { _n: 0 }) as Box<dyn Tool>
        ]));

        // Spawn N concurrent swaps
        let mut handles = vec![];
        let n = 20;
        for i in 0..n {
            let mgr_clone = Arc::clone(&mgr);
            let handle = tokio::spawn(async move {
                let tools = vec![Box::new(DummyTool { _n: i }) as Box<dyn Tool>];
                mgr_clone.atomic_swap(tools)
            });
            handles.push(handle);
        }

        let mut versions: Vec<u64> = vec![];
        for handle in handles {
            versions.push(handle.await.unwrap());
        }

        // Version should be monotonically increasing (1 + n swaps = n + 1)
        assert_eq!(mgr.version(), (n + 1) as u64);

        // All returned versions should be unique (no duplicates from races)
        versions.sort();
        versions.dedup();
        assert_eq!(versions.len(), n);
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
