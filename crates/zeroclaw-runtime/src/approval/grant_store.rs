//! Persistent storage for per-(channel, topic, user, tool) approval grants.
//!
//! See spec: docs/superpowers/specs/2026-06-18-persistent-tool-approval-grants-design.md

use anyhow::Context;
use lru::LruCache;
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalGrant {
    pub id: String,
    pub channel_ref: String,
    pub topic: Option<String>,
    pub user_master_id: String,
    pub tool_name: String,
    pub granted_at: i64,
    pub granted_by_master_id: String,
    pub granted_via_channel: String,
}

impl ApprovalGrant {
    /// Construct a new grant with a fresh UUID v4 id and the current UTC second.
    pub fn new(
        channel_ref: String,
        topic: Option<String>,
        user_master_id: String,
        tool_name: String,
        granted_by_master_id: String,
        granted_via_channel: String,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            channel_ref,
            topic,
            user_master_id,
            tool_name,
            granted_at: chrono::Utc::now().timestamp(),
            granted_by_master_id,
            granted_via_channel,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GrantFilter {
    pub channel_ref: Option<String>,
    pub topic: Option<Option<String>>, // double-Option: outer = "filter applied?", inner = topic value
    pub user_master_id: Option<String>,
    pub tool_name: Option<String>,
}

pub trait ApprovalGrantStore: Send + Sync {
    fn get(
        &self,
        channel_ref: &str,
        topic: Option<&str>,
        user_master_id: &str,
        tool_name: &str,
    ) -> anyhow::Result<Option<ApprovalGrant>>;

    fn put(&self, grant: ApprovalGrant) -> anyhow::Result<()>;

    fn list(&self, filter: &GrantFilter) -> anyhow::Result<Vec<ApprovalGrant>>;

    fn delete(&self, grant_id: &str) -> anyhow::Result<bool>;
}

type CacheKey = (String, Option<String>, String, String);

pub struct SqliteGrantStore {
    conn: Mutex<Connection>,
    cache: Mutex<LruCache<CacheKey, Option<ApprovalGrant>>>,
}

impl SqliteGrantStore {
    pub fn new(workspace_dir: &Path) -> anyhow::Result<Self> {
        let state_dir = workspace_dir.join("state");
        let _ = std::fs::create_dir_all(&state_dir);
        let db_path = state_dir.join("approval_grants.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open approval_grants.db at {}", db_path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS approval_grants (
                id                     TEXT PRIMARY KEY,
                channel_ref            TEXT NOT NULL,
                topic                  TEXT,
                user_master_id         TEXT NOT NULL,
                tool_name              TEXT NOT NULL,
                granted_at             INTEGER NOT NULL,
                granted_by_master_id   TEXT NOT NULL,
                granted_via_channel    TEXT NOT NULL,
                UNIQUE (channel_ref, topic, user_master_id, tool_name)
             );
             CREATE INDEX IF NOT EXISTS idx_approval_grants_lookup
                ON approval_grants (channel_ref, topic, user_master_id, tool_name);
             CREATE INDEX IF NOT EXISTS idx_approval_grants_user
                ON approval_grants (user_master_id);",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())),
        })
    }
}

fn row_to_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalGrant> {
    Ok(ApprovalGrant {
        id: row.get(0)?,
        channel_ref: row.get(1)?,
        topic: row.get(2)?,
        user_master_id: row.get(3)?,
        tool_name: row.get(4)?,
        granted_at: row.get(5)?,
        granted_by_master_id: row.get(6)?,
        granted_via_channel: row.get(7)?,
    })
}

impl ApprovalGrantStore for SqliteGrantStore {
    fn get(
        &self,
        channel_ref: &str,
        topic: Option<&str>,
        user_master_id: &str,
        tool_name: &str,
    ) -> anyhow::Result<Option<ApprovalGrant>> {
        let key: CacheKey = (
            channel_ref.to_string(),
            topic.map(str::to_string),
            user_master_id.to_string(),
            tool_name.to_string(),
        );
        if let Some(cached) = self.cache.lock().get(&key).cloned() {
            return Ok(cached);
        }
        let conn = self.conn.lock();
        let row = match topic {
            Some(t) => conn
                .query_row(
                    "SELECT id, channel_ref, topic, user_master_id, tool_name, \
                            granted_at, granted_by_master_id, granted_via_channel \
                     FROM approval_grants \
                     WHERE channel_ref = ?1 AND topic = ?2 \
                       AND user_master_id = ?3 AND tool_name = ?4",
                    params![channel_ref, t, user_master_id, tool_name],
                    row_to_grant,
                )
                .optional()?,
            None => conn
                .query_row(
                    "SELECT id, channel_ref, topic, user_master_id, tool_name, \
                            granted_at, granted_by_master_id, granted_via_channel \
                     FROM approval_grants \
                     WHERE channel_ref = ?1 AND topic IS NULL \
                       AND user_master_id = ?2 AND tool_name = ?3",
                    params![channel_ref, user_master_id, tool_name],
                    row_to_grant,
                )
                .optional()?,
        };
        self.cache.lock().put(key, row.clone());
        Ok(row)
    }

    fn put(&self, grant: ApprovalGrant) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO approval_grants \
                (id, channel_ref, topic, user_master_id, tool_name, \
                 granted_at, granted_by_master_id, granted_via_channel) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(channel_ref, topic, user_master_id, tool_name) \
             DO UPDATE SET granted_at = excluded.granted_at, \
                           granted_by_master_id = excluded.granted_by_master_id, \
                           granted_via_channel = excluded.granted_via_channel",
            params![
                grant.id,
                grant.channel_ref,
                grant.topic,
                grant.user_master_id,
                grant.tool_name,
                grant.granted_at,
                grant.granted_by_master_id,
                grant.granted_via_channel,
            ],
        )?;
        drop(conn);
        let key: CacheKey = (
            grant.channel_ref.clone(),
            grant.topic.clone(),
            grant.user_master_id.clone(),
            grant.tool_name.clone(),
        );
        self.cache.lock().pop(&key); // invalidate; subsequent get reloads
        Ok(())
    }

    fn list(&self, filter: &GrantFilter) -> anyhow::Result<Vec<ApprovalGrant>> {
        let mut sql = String::from(
            "SELECT id, channel_ref, topic, user_master_id, tool_name, \
                    granted_at, granted_by_master_id, granted_via_channel \
             FROM approval_grants WHERE 1=1",
        );
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(c) = &filter.channel_ref {
            sql.push_str(" AND channel_ref = ?");
            args.push(c.clone().into());
        }
        if let Some(t_outer) = &filter.topic {
            match t_outer {
                Some(t) => {
                    sql.push_str(" AND topic = ?");
                    args.push(t.clone().into());
                }
                None => sql.push_str(" AND topic IS NULL"),
            }
        }
        if let Some(u) = &filter.user_master_id {
            sql.push_str(" AND user_master_id = ?");
            args.push(u.clone().into());
        }
        if let Some(tool) = &filter.tool_name {
            sql.push_str(" AND tool_name = ?");
            args.push(tool.clone().into());
        }
        sql.push_str(" ORDER BY granted_at DESC");

        let conn = self.conn.lock();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), row_to_grant)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn delete(&self, grant_id: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "DELETE FROM approval_grants WHERE id = ?1",
            params![grant_id],
        )?;
        drop(conn);
        // Cache is keyed by (channel,topic,user,tool); we don't know which key this
        // id maps to without an extra query. Cheap correct fix: clear the whole cache.
        self.cache.lock().clear();
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, SqliteGrantStore) {
        let tmp = TempDir::new().unwrap();
        let s = SqliteGrantStore::new(tmp.path()).unwrap();
        (tmp, s)
    }

    fn grant(channel: &str, topic: Option<&str>, user: &str, tool: &str) -> ApprovalGrant {
        ApprovalGrant::new(
            channel.into(),
            topic.map(str::to_string),
            user.into(),
            tool.into(),
            "u_admin".into(),
            channel.into(),
        )
    }

    #[test]
    fn round_trip_with_topic_some() {
        let (_t, s) = store();
        let g = grant("dawnim.work", Some("db_lock"), "u_alice", "shell");
        s.put(g.clone()).unwrap();
        let got = s
            .get("dawnim.work", Some("db_lock"), "u_alice", "shell")
            .unwrap()
            .unwrap();
        assert_eq!(got.id, g.id);
        assert_eq!(got.granted_by_master_id, "u_admin");
    }

    #[test]
    fn round_trip_with_topic_none() {
        let (_t, s) = store();
        let g = grant("dawnim.work", None, "u_alice", "shell");
        s.put(g.clone()).unwrap();
        assert_eq!(
            s.get("dawnim.work", None, "u_alice", "shell")
                .unwrap()
                .unwrap()
                .id,
            g.id
        );
    }

    #[test]
    fn topic_none_and_topic_empty_string_are_distinct() {
        let (_t, s) = store();
        s.put(grant("dawnim.work", None, "u_alice", "shell"))
            .unwrap();
        s.put(grant("dawnim.work", Some(""), "u_alice", "shell"))
            .unwrap();
        assert!(
            s.get("dawnim.work", None, "u_alice", "shell")
                .unwrap()
                .is_some()
        );
        assert!(
            s.get("dawnim.work", Some(""), "u_alice", "shell")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn upsert_refreshes_granted_at_keeps_row_count_one() {
        let (_t, s) = store();
        let g1 = grant("dawnim.work", Some("db_lock"), "u_alice", "shell");
        let mut g2 = grant("dawnim.work", Some("db_lock"), "u_alice", "shell");
        g2.granted_at = g1.granted_at + 60;
        g2.granted_by_master_id = "u_admin2".into();
        s.put(g1.clone()).unwrap();
        s.put(g2.clone()).unwrap();
        let all = s.list(&GrantFilter::default()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].granted_by_master_id, "u_admin2");
        assert_eq!(all[0].granted_at, g1.granted_at + 60);
    }

    #[test]
    fn list_filter_combinations() {
        let (_t, s) = store();
        s.put(grant("dawnim.work", Some("t1"), "u_alice", "shell"))
            .unwrap();
        s.put(grant("dawnim.work", Some("t2"), "u_alice", "shell"))
            .unwrap();
        s.put(grant("dawnim.work", Some("t1"), "u_bob", "file_write"))
            .unwrap();

        assert_eq!(s.list(&GrantFilter::default()).unwrap().len(), 3);
        assert_eq!(
            s.list(&GrantFilter {
                tool_name: Some("shell".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            s.list(&GrantFilter {
                user_master_id: Some("u_bob".into()),
                ..Default::default()
            })
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn list_orders_by_granted_at_desc() {
        let (_t, s) = store();
        let mut older = grant("dawnim.work", Some("t1"), "u_alice", "shell");
        older.granted_at = 1000;
        s.put(older).unwrap();
        let mut newer = grant("dawnim.work", Some("t2"), "u_alice", "shell");
        newer.granted_at = 2000;
        s.put(newer).unwrap();
        let all = s.list(&GrantFilter::default()).unwrap();
        assert_eq!(all[0].granted_at, 2000);
        assert_eq!(all[1].granted_at, 1000);
    }

    #[test]
    fn delete_existing_returns_true() {
        let (_t, s) = store();
        let g = grant("dawnim.work", Some("t1"), "u_alice", "shell");
        let id = g.id.clone();
        s.put(g).unwrap();
        assert!(s.delete(&id).unwrap());
        assert!(
            s.get("dawnim.work", Some("t1"), "u_alice", "shell")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn delete_missing_returns_false() {
        let (_t, s) = store();
        assert!(!s.delete("nonexistent-id").unwrap());
    }

    #[test]
    fn cache_hit_on_repeated_get() {
        let (_t, s) = store();
        s.put(grant("dawnim.work", Some("t1"), "u_alice", "shell"))
            .unwrap();
        let _ = s
            .get("dawnim.work", Some("t1"), "u_alice", "shell")
            .unwrap();
        let cached_key: CacheKey = (
            "dawnim.work".into(),
            Some("t1".into()),
            "u_alice".into(),
            "shell".into(),
        );
        assert!(s.cache.lock().get(&cached_key).is_some());
    }

    #[test]
    fn cache_invalidated_on_put() {
        let (_t, s) = store();
        let g = grant("dawnim.work", Some("t1"), "u_alice", "shell");
        s.put(g.clone()).unwrap();
        let _ = s
            .get("dawnim.work", Some("t1"), "u_alice", "shell")
            .unwrap();
        s.put(g).unwrap();
        let key: CacheKey = (
            "dawnim.work".into(),
            Some("t1".into()),
            "u_alice".into(),
            "shell".into(),
        );
        // After put, cache should not contain the key (next get reloads).
        assert!(s.cache.lock().peek(&key).is_none());
    }

    #[test]
    fn get_returns_none_for_missing_key_and_caches_none() {
        let (_t, s) = store();
        assert!(
            s.get("dawnim.work", Some("t1"), "u_alice", "shell")
                .unwrap()
                .is_none()
        );
        let key: CacheKey = (
            "dawnim.work".into(),
            Some("t1".into()),
            "u_alice".into(),
            "shell".into(),
        );
        let cached = s.cache.lock().peek(&key).cloned();
        assert!(cached.is_some()); // outer Some
        assert!(cached.unwrap().is_none()); // inner None
    }

    #[test]
    fn grant_survives_reopen() {
        let tmp = TempDir::new().unwrap();
        {
            let s = SqliteGrantStore::new(tmp.path()).unwrap();
            s.put(grant("dawnim.work", Some("t1"), "u_alice", "shell"))
                .unwrap();
        }
        let s2 = SqliteGrantStore::new(tmp.path()).unwrap();
        assert!(
            s2.get("dawnim.work", Some("t1"), "u_alice", "shell")
                .unwrap()
                .is_some()
        );
    }
}
