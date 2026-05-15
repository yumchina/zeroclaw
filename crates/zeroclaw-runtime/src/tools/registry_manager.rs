//! Thread-safe tool registry with atomic swap support for hot reloading.
//!
//! The ToolRegistryManager holds tools in an Arc<Vec<Box<dyn Tool>>>,
//! allowing atomic replacement without locking. Agent sessions clone
//! the Arc and naturally observe new tools after a swap.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use zeroclaw_api::Tool;

/// Thread-safe tool registry with atomic swap capability.
///
/// # Example
/// ```rust
/// let mgr = ToolRegistryManager::new(vec![tool1, tool2]);
/// let current = mgr.get_current(); // Arc<Vec<Box<dyn Tool>>>
/// mgr.atomic_swap(vec![tool3]); // Atomic replacement
/// ```
pub struct ToolRegistryManager {
    /// The current tool registry. Wrapped in Arc for cheap cloning.
    inner: Arc<Vec<Box<dyn Tool>>>,

    /// Monotonically increasing version number for each swap.
    /// Useful for debugging and event logging.
    version: AtomicU64,
}

impl ToolRegistryManager {
    /// Create a new ToolRegistryManager with the given tools.
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self {
        tracing::info!("Creating ToolRegistryManager with {} tools, version 1", tools.len());
        Self {
            inner: Arc::new(tools),
            version: AtomicU64::new(1),
        }
    }

    /// Atomically replace the current tools with a new set.
    ///
    /// Returns the new version number. Old Arc references remain valid
    /// until all holders drop them.
    pub fn atomic_swap(&self, new_tools: Vec<Box<dyn Tool>>) -> u64 {
        let new_arc = Arc::new(new_tools);
        // SAFETY: This is a lock-free atomic swap of Arc pointers.
        // The old Arc is returned and will be dropped when all references are released.
        let old_arc = std::mem::replace(&mut self.inner, new_arc);
        let new_version = self.version.fetch_add(1, Ordering::SeqCst) + 1;

        tracing::info!(
            "Tool registry swapped: version {}, {} tools, {} old refs pending drop",
            new_version,
            self.inner.len(),
            Arc::strong_count(&old_arc)
        );

        new_version
    }

    /// Get a clone of the current tools Arc.
    ///
    /// This is a cheap pointer-sized clone. The returned Arc can be
    /// freely passed between threads/tasks.
    pub fn get_current(&self) -> Arc<Vec<Box<dyn Tool>>> {
        Arc::clone(&self.inner)
    }

    /// Get the current version number.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Get the current number of tools.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool;

    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }

        fn description(&self) -> &str {
            "A dummy tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<zeroclaw_api::ToolResult> {
            Ok(zeroclaw_api::ToolResult {
                success: true,
                output: "dummy".to_string(),
                error: None,
            })
        }
    }

    #[test]
    fn test_registry_manager_creation() {
        let tools = vec![Box::new(DummyTool) as Box<dyn Tool>];
        let mgr = ToolRegistryManager::new(tools);
        assert_eq!(mgr.version(), 1);
        assert_eq!(mgr.len(), 1);
        assert!(!mgr.is_empty());
    }

    #[test]
    fn test_atomic_swap_increments_version() {
        let tools = vec![Box::new(DummyTool) as Box<dyn Tool>];
        let mgr = ToolRegistryManager::new(tools);

        let v1 = mgr.version();
        let v2 = mgr.atomic_swap(vec![]);
        let v3 = mgr.version();

        assert_eq!(v1, 1);
        assert_eq!(v2, 2);
        assert_eq!(v3, 2);
    }

    #[test]
    fn test_get_current_returns_arc() {
        let tools = vec![Box::new(DummyTool) as Box<dyn Tool>];
        let mgr = ToolRegistryManager::new(tools);

        let arc1 = mgr.get_current();
        let arc2 = mgr.get_current();

        // Same Arc because no swap occurred
        assert!(Arc::ptr_eq(&arc1, &arc2));
    }

    #[test]
    fn test_swap_creates_new_arc() {
        let tools = vec![Box::new(DummyTool) as Box<dyn Tool>];
        let mgr = ToolRegistryManager::new(tools);

        let arc1 = mgr.get_current();
        mgr.atomic_swap(vec![]);
        let arc2 = mgr.get_current();

        // Different Arc after swap
        assert!(!Arc::ptr_eq(&arc1, &arc2));
    }

    #[test]
    fn test_old_arc_remains_valid() {
        let tools = vec![Box::new(DummyTool) as Box<dyn Tool>];
        let mgr = ToolRegistryManager::new(tools);

        let old_arc = mgr.get_current();
        assert_eq!(old_arc.len(), 1);

        mgr.atomic_swap(vec![]);

        // Old Arc still has the old data
        assert_eq!(old_arc.len(), 1);
        // New Arc is empty
        assert_eq!(mgr.get_current().len(), 0);
        assert!(mgr.is_empty());
    }

    #[tokio::test::async_trait]
    async fn test_concurrent_swap() {
        use std::sync::Arc as StdArc;
        let mgr = StdArc::new(ToolRegistryManager::new(vec![
            Box::new(DummyTool) as Box<dyn Tool>
        ]));

        let mut handles = vec![];
        for i in 0..10 {
            let mgr_clone = StdArc::clone(&mgr);
            let handle = tokio::spawn(async move {
                let tools = vec![
                    Box::new(DummyTool) as Box<dyn Tool>
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
