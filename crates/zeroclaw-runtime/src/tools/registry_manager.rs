//! Thread-safe tool registry with atomic swap support for hot reloading.
//!
//! The ToolRegistryManager holds tools in an Arc<Vec<Box<dyn Tool>>>,
//! allowing atomic replacement without locking. Agent sessions clone
//! the Arc and naturally observe new tools after a swap.
//!
//! ## Daemon-level singleton
//!
//! For daemon mode (gateway + channels + heartbeat), a global singleton
//! is used so all components share the same registry and hot reload
//! updates are visible everywhere.

use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use crate::tools::Tool;

/// Global singleton for daemon mode.
/// Initialized once at daemon startup, shared by gateway/channels/heartbeat.
static GLOBAL_REGISTRY: OnceLock<Arc<ToolRegistryManager>> = OnceLock::new();

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
    inner: Mutex<Arc<Vec<Box<dyn Tool>>>>,

    /// Monotonically increasing version number for each swap.
    /// Useful for debugging and event logging.
    version: AtomicU64,
}

impl ToolRegistryManager {
    /// Create a new ToolRegistryManager with the given tools.
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self {
        tracing::info!("Creating ToolRegistryManager with {} tools, version 1", tools.len());
        Self {
            inner: Mutex::new(Arc::new(tools)),
            version: AtomicU64::new(1),
        }
    }

    /// Atomically replace the current tools with a new set.
    ///
    /// Returns the new version number. Old Arc references remain valid
    /// until all holders drop them.
    pub fn atomic_swap(&self, new_tools: Vec<Box<dyn Tool>>) -> u64 {
        let new_arc = Arc::new(new_tools);
        let new_len = new_arc.len();
        let old_arc = std::mem::replace(&mut *self.inner.lock().unwrap(), new_arc);
        let new_version = self.version.fetch_add(1, Ordering::SeqCst) + 1;

        tracing::info!(
            "Tool registry swapped: version {}, {} tools, {} old refs pending drop",
            new_version,
            new_len,
            Arc::strong_count(&old_arc)
        );

        new_version
    }

    /// Get a clone of the current tools Arc.
    ///
    /// This is a cheap pointer-sized clone. The returned Arc can be
    /// freely passed between threads/tasks.
    pub fn get_current(&self) -> Arc<Vec<Box<dyn Tool>>> {
        Arc::clone(&self.inner.lock().unwrap())
    }

    /// Get the current version number.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }

    /// Get the current number of tools.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }
}

impl std::fmt::Debug for ToolRegistryManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistryManager")
            .field("version", &self.version.load(Ordering::SeqCst))
            .field("tool_count", &self.inner.lock().unwrap().len())
            .finish()
    }
}

// ── Global singleton API for daemon mode ─────────────────────────────────────

/// Initialize the global tool registry manager for daemon mode.
///
/// Should be called once at daemon startup before spawning gateway/channels.
/// Returns the Arc to the registry manager for passing to components.
///
/// # Panics
///
/// Panics if called more than once (daemon restart should use reload signal).
pub fn init_global_registry(tools: Vec<Box<dyn Tool>>) -> Arc<ToolRegistryManager> {
    let mgr = Arc::new(ToolRegistryManager::new(tools));
    GLOBAL_REGISTRY.set(Arc::clone(&mgr))
        .expect("Global registry already initialized — daemon should use reload instead of re-init");
    tracing::info!("Global tool registry initialized with {} tools", mgr.len());
    mgr
}

/// Get the global tool registry manager.
///
/// Returns `None` if not initialized (e.g. CLI mode, or daemon not yet started).
pub fn get_global_registry() -> Option<Arc<ToolRegistryManager>> {
    GLOBAL_REGISTRY.get().cloned()
}

/// Get current tools from global registry.
///
/// Convenience function for components that just need the tools.
/// Returns an empty Vec if global registry not initialized.
pub fn get_global_tools() -> Arc<Vec<Box<dyn Tool>>> {
    GLOBAL_REGISTRY
        .get()
        .map(|mgr| mgr.get_current())
        .unwrap_or_else(|| Arc::new(Vec::new()))
}

/// Swap tools in global registry (used by hot reload watcher).
///
/// Returns new version number, or 0 if global registry not initialized.
pub fn swap_global_registry(new_tools: Vec<Box<dyn Tool>>) -> u64 {
    GLOBAL_REGISTRY
        .get()
        .map(|mgr| mgr.atomic_swap(new_tools))
        .unwrap_or(0)
}

/// RAII guard that aborts watcher tasks on drop.
/// Used by daemon to ensure watcher cleanup on shutdown.
pub struct WatcherGuard(Mutex<Vec<tokio::task::JoinHandle<()>>>);

impl WatcherGuard {
    pub fn new() -> Self {
        Self(Mutex::new(Vec::new()))
    }

    pub fn push(&self, handle: tokio::task::JoinHandle<()>) {
        self.0.lock().unwrap().push(handle);
    }

    pub fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }
}

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        let handles = self.0.lock().unwrap().drain(..).collect::<Vec<_>>();
        for h in &handles {
            h.abort();
        }
        tracing::info!("WatcherGuard dropped, aborted {} watcher tasks", handles.len());
    }
}

// Global watcher guard for daemon mode
static GLOBAL_WATCHER_GUARD: OnceLock<WatcherGuard> = OnceLock::new();

/// Initialize global watcher guard.
/// Should be called once at daemon startup.
pub fn init_global_watcher_guard() {
    GLOBAL_WATCHER_GUARD.get_or_init(WatcherGuard::new);
}

/// Push a watcher handle to the global guard.
pub fn push_global_watcher_handle(handle: tokio::task::JoinHandle<()>) {
    if let Some(guard) = GLOBAL_WATCHER_GUARD.get() {
        guard.push(handle);
    } else {
        tracing::warn!("Global watcher guard not initialized, handle will not be tracked");
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

    #[tokio::test]
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