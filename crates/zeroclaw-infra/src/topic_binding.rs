//! Cross-channel topic binding registry.
//!
//! Maps a (channel_ref, sender) pair to a master-channel topic id. Used by
//! the orchestrator's `/topic` slash command to let a superuser on a
//! channel without native topic support (e.g. feishu) route their
//! subsequent messages into a specific DawnIM topic's unified session.
//!
//! In-memory `HashMap` plus best-effort JSON persistence to
//! `{data_dir}/sessions/topic_binding.json`. Persistence is a Task 2 add-on.

use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingKey {
    pub channel_ref: String,
    pub sender: String,
}

pub struct TopicBindingRegistry {
    bindings: RwLock<HashMap<BindingKey, String>>,
    #[allow(dead_code)] // Task 2 wires persistence; field reserved.
    persist_path: PathBuf,
}

impl TopicBindingRegistry {
    /// Construct an empty registry that will persist to `persist_path` on
    /// mutation. Does not touch the filesystem.
    pub fn new(persist_path: PathBuf) -> Self {
        Self {
            bindings: RwLock::new(HashMap::new()),
            persist_path,
        }
    }

    pub fn get(&self, channel_ref: &str, sender: &str) -> Option<String> {
        let key = BindingKey {
            channel_ref: channel_ref.to_string(),
            sender: sender.to_string(),
        };
        self.bindings.read().get(&key).cloned()
    }

    pub fn set(&self, channel_ref: &str, sender: &str, topic_id: &str) {
        let key = BindingKey {
            channel_ref: channel_ref.to_string(),
            sender: sender.to_string(),
        };
        self.bindings.write().insert(key, topic_id.to_string());
    }

    pub fn clear(&self, channel_ref: &str, sender: &str) -> bool {
        let key = BindingKey {
            channel_ref: channel_ref.to_string(),
            sender: sender.to_string(),
        };
        self.bindings.write().remove(&key).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> TopicBindingRegistry {
        TopicBindingRegistry::new(PathBuf::from("/tmp/never-touched-in-task-1"))
    }

    #[test]
    fn get_returns_none_when_unset() {
        let r = reg();
        assert_eq!(r.get("feishu.work", "u_alice"), None);
    }

    #[test]
    fn set_then_get_roundtrips() {
        let r = reg();
        r.set("feishu.work", "u_alice", "db_lock");
        assert_eq!(r.get("feishu.work", "u_alice"), Some("db_lock".to_string()));
    }

    #[test]
    fn set_overwrites_existing() {
        let r = reg();
        r.set("feishu.work", "u_alice", "db_lock");
        r.set("feishu.work", "u_alice", "migrations");
        assert_eq!(
            r.get("feishu.work", "u_alice"),
            Some("migrations".to_string())
        );
    }

    #[test]
    fn clear_removes_and_returns_true() {
        let r = reg();
        r.set("feishu.work", "u_alice", "db_lock");
        assert!(r.clear("feishu.work", "u_alice"));
        assert_eq!(r.get("feishu.work", "u_alice"), None);
    }

    #[test]
    fn clear_returns_false_when_absent() {
        let r = reg();
        assert!(!r.clear("feishu.work", "u_alice"));
    }

    #[test]
    fn bindings_are_per_channel_sender_pair() {
        let r = reg();
        r.set("feishu.work", "u_alice", "topic_a");
        r.set("feishu.work", "u_bob", "topic_b");
        r.set("wecom_ws.default", "u_alice", "topic_c");
        assert_eq!(r.get("feishu.work", "u_alice"), Some("topic_a".to_string()));
        assert_eq!(r.get("feishu.work", "u_bob"), Some("topic_b".to_string()));
        assert_eq!(
            r.get("wecom_ws.default", "u_alice"),
            Some("topic_c".to_string())
        );
    }
}
