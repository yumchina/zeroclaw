//! Task-local URL allowlist for `scrub_credentials_with_allowlist` callers.

use std::sync::Arc;

use crate::security::AllowlistRule;

tokio::task_local! {
    pub static TOOL_LOOP_ALLOWLIST: Option<Arc<Vec<AllowlistRule>>>;
}

/// Snapshot of the current task-local allowlist. Returns an empty vec when
/// the scope is unset, so callers can pass `&[]` semantics without
/// branching on the option.
pub fn current_allowlist() -> Vec<AllowlistRule> {
    TOOL_LOOP_ALLOWLIST
        .try_with(|slot| {
            slot.as_ref()
                .map(|arc| arc.as_ref().clone())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_allowlist_returns_empty_when_unset() {
        assert!(current_allowlist().is_empty());
    }

    #[tokio::test]
    async fn current_allowlist_returns_rules_inside_scope() {
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let arc = Arc::new(vec![rule]);
        TOOL_LOOP_ALLOWLIST
            .scope(Some(arc.clone()), async {
                let got = current_allowlist();
                assert_eq!(got.len(), 1);
            })
            .await;
    }

    #[tokio::test]
    async fn current_allowlist_returns_empty_when_scope_value_is_none() {
        TOOL_LOOP_ALLOWLIST
            .scope(None, async {
                assert!(current_allowlist().is_empty());
            })
            .await;
    }
}
