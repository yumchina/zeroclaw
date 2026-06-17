//! Cross-channel topic binding registry.
//!
//! Maps a (channel_ref, sender) pair to a master-channel topic id. Used by
//! the orchestrator's `/topic` slash command to let a superuser on a
//! channel without native topic support (e.g. feishu) route their
//! subsequent messages into a specific DawnIM topic's unified session.
//!
//! In-memory `HashMap` plus best-effort JSON persistence to
//! `{data_dir}/sessions/topic_binding.json`. A mutation triggers a
//! synchronous rewrite; write failures degrade to a warning so a
//! transient disk error never blocks the user's command.

use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingKey {
    pub channel_ref: String,
    pub sender: String,
}

pub struct TopicBindingRegistry {
    bindings: RwLock<HashMap<BindingKey, String>>,
    persist_path: PathBuf,
}

impl TopicBindingRegistry {
    /// Open the registry at `persist_path`. If the file is missing or
    /// unparseable, returns an empty registry (with a warning log for the
    /// parse-failure case). Filesystem permission errors are the only
    /// hard failure surfaced to the caller.
    pub fn load(persist_path: PathBuf) -> io::Result<Self> {
        let bindings = match std::fs::read(&persist_path) {
            Ok(bytes) => Self::parse_bytes(&bytes, &persist_path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            bindings: RwLock::new(bindings),
            persist_path,
        })
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
        let mut guard = self.bindings.write();
        guard.insert(key, topic_id.to_string());
        self.persist_locked(&guard);
    }

    pub fn clear(&self, channel_ref: &str, sender: &str) -> bool {
        let key = BindingKey {
            channel_ref: channel_ref.to_string(),
            sender: sender.to_string(),
        };
        let mut guard = self.bindings.write();
        let removed = guard.remove(&key).is_some();
        if removed {
            self.persist_locked(&guard);
        }
        removed
    }

    fn parse_bytes(bytes: &[u8], path: &Path) -> HashMap<BindingKey, String> {
        let map: BTreeMap<String, String> = match serde_json::from_slice(bytes) {
            Ok(m) => m,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "path": path.display().to_string(),
                            "error": e.to_string(),
                        })),
                    "topic_binding.json parse failed; starting empty"
                );
                return HashMap::new();
            }
        };
        let mut out = HashMap::with_capacity(map.len());
        for (raw_key, topic) in map {
            // Split on the FIRST '|' only. Entries with extra '|' in the
            // sender portion are malformed (sender ids must not contain
            // '|') and are skipped.
            let mut parts = raw_key.splitn(2, '|');
            let channel_ref = parts.next().unwrap_or("");
            let sender = parts.next().unwrap_or("");
            if channel_ref.is_empty() || sender.is_empty() || sender.contains('|') {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"raw_key": raw_key})),
                    "topic_binding entry has malformed key; skipping"
                );
                continue;
            }
            out.insert(
                BindingKey {
                    channel_ref: channel_ref.to_string(),
                    sender: sender.to_string(),
                },
                topic,
            );
        }
        out
    }

    /// Best-effort sync write under the write lock. Failure is logged, not
    /// propagated, so a transient disk error never blocks the user's
    /// command; the next successful write restores consistency.
    fn persist_locked(&self, snapshot: &HashMap<BindingKey, String>) {
        if let Some(parent) = self.persist_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "parent": parent.display().to_string(),
                        "error": e.to_string(),
                    })),
                "topic_binding mkdir failed"
            );
            return;
        }
        let map: BTreeMap<String, &String> = snapshot
            .iter()
            .map(|(k, v)| (format!("{}|{}", k.channel_ref, k.sender), v))
            .collect();
        let bytes = match serde_json::to_vec_pretty(&map) {
            Ok(b) => b,
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": e.to_string()})),
                    "topic_binding serialize failed"
                );
                return;
            }
        };
        if let Err(e) = std::fs::write(&self.persist_path, &bytes) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "path": self.persist_path.display().to_string(),
                        "error": e.to_string(),
                    })),
                "topic_binding write failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Helper: an empty registry backed by a temp directory whose lifetime
    /// outlives the test. Suitable for short-lived unit tests.
    fn reg() -> TopicBindingRegistry {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sessions").join("topic_binding.json");
        // Leak the TempDir for the duration of the test; the OS reclaims
        // the directory when the process exits.
        let _ = Box::leak(Box::new(tmp));
        TopicBindingRegistry::load(path).unwrap()
    }

    fn temp_path() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sessions").join("topic_binding.json");
        (tmp, path)
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

    #[test]
    fn load_from_missing_file_yields_empty_registry() {
        let (_tmp, path) = temp_path();
        let r = TopicBindingRegistry::load(path).unwrap();
        assert_eq!(r.get("feishu.work", "u_alice"), None);
    }

    #[test]
    fn set_persists_to_disk_and_load_restores() {
        let (_tmp, path) = temp_path();
        {
            let r = TopicBindingRegistry::load(path.clone()).unwrap();
            r.set("feishu.work", "u_alice", "db_lock");
        }
        let r2 = TopicBindingRegistry::load(path).unwrap();
        assert_eq!(
            r2.get("feishu.work", "u_alice"),
            Some("db_lock".to_string())
        );
    }

    #[test]
    fn clear_persists_removal() {
        let (_tmp, path) = temp_path();
        {
            let r = TopicBindingRegistry::load(path.clone()).unwrap();
            r.set("feishu.work", "u_alice", "db_lock");
            assert!(r.clear("feishu.work", "u_alice"));
        }
        let r2 = TopicBindingRegistry::load(path).unwrap();
        assert_eq!(r2.get("feishu.work", "u_alice"), None);
    }

    #[test]
    fn corrupt_json_yields_empty_registry_not_error() {
        let (_tmp, path) = temp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not valid json").unwrap();
        let r = TopicBindingRegistry::load(path).unwrap();
        assert_eq!(r.get("feishu.work", "u_alice"), None);
    }

    #[test]
    fn entries_with_extra_pipe_separator_are_skipped() {
        let (_tmp, path) = temp_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            br#"{"feishu.work|u_alice":"db_lock","bad|key|extra":"x"}"#,
        )
        .unwrap();
        let r = TopicBindingRegistry::load(path).unwrap();
        assert_eq!(r.get("feishu.work", "u_alice"), Some("db_lock".to_string()));
    }
}
