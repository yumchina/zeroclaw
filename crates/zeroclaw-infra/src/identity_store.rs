//! Cross-channel identity store for unified sessions.
//!
//! Backs the "same person across channels" feature: a single SQLite DB
//! (`{workspace}/sessions/identity.db`) holds (channel_ref, sender) → master_id
//! mappings plus a whitelist of master ids that have unified sessions enabled.
//! Paired `/bind` codes are short-lived and live in memory only.

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Time-to-live for a `/bind` pairing code.
const BIND_CODE_TTL: Duration = Duration::from_secs(300);

/// Resolves a channel message's (channel_ref, sender) to a unified `master_id`,
/// and manages `/bind` pairing. All methods are infallible at the call site
/// (errors degrade to "no mapping") so a broken identity DB never blocks chat.
pub trait IdentityResolver: Send + Sync {
    /// Master-channel message: `Some(sender)` iff `sender` is a whitelisted
    /// master id. Slave-channel message: `Some(master_id)` iff a binding exists
    /// and that master id is whitelisted. Otherwise `None`.
    fn resolve(&self, channel_ref: &str, sender: &str, is_master: bool) -> Option<String>;

    /// Issue a one-time pairing code for a master user. `None` if `master_id`
    /// is not a whitelisted master (only superusers may initiate binding).
    fn issue_code(&self, master_id: &str) -> Option<String>;

    /// Redeem a pairing code from a slave channel, writing the mapping.
    /// `Ok(master_id)` on success; `Err(reason)` if the code is unknown/expired.
    fn redeem_code(&self, code: &str, channel_ref: &str, sender: &str) -> Result<String, String>;

    /// Remove a slave-channel binding. Returns `true` if a row was deleted.
    fn unbind(&self, channel_ref: &str, sender: &str) -> bool;
}

/// SQLite-backed identity store. Pairing codes are in-memory only.
pub struct SqliteIdentityStore {
    conn: Mutex<Connection>,
    /// code -> (master_id, issued_at)
    codes: Mutex<HashMap<String, (String, SystemTime)>>,
}

impl SqliteIdentityStore {
    /// Open or create `{workspace}/sessions/identity.db`.
    pub fn new(workspace_dir: &Path) -> rusqlite::Result<Self> {
        let sessions_dir = workspace_dir.join("sessions");
        // Best-effort: directory may already exist (shared with sessions.db).
        let _ = std::fs::create_dir_all(&sessions_dir);
        let db_path = sessions_dir.join("identity.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS identity_mapping (
                channel_ref TEXT NOT NULL,
                sender      TEXT NOT NULL,
                master_id   TEXT NOT NULL,
                PRIMARY KEY (channel_ref, sender)
             );
             CREATE INDEX IF NOT EXISTS idx_identity_master
                ON identity_mapping(master_id);
             CREATE TABLE IF NOT EXISTS unified_member (
                master_id TEXT PRIMARY KEY
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            codes: Mutex::new(HashMap::new()),
        })
    }

    /// Insert each id into the whitelist. Idempotent (INSERT OR IGNORE).
    pub fn seed_superusers(&self, superusers: &[String]) -> rusqlite::Result<()> {
        let conn = self.conn.lock();
        for id in superusers {
            conn.execute(
                "INSERT OR IGNORE INTO unified_member (master_id) VALUES (?1)",
                params![id],
            )?;
        }
        Ok(())
    }

    fn is_whitelisted(conn: &Connection, master_id: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM unified_member WHERE master_id = ?1 LIMIT 1",
            params![master_id],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// 6-digit code derived from the nanosecond clock plus a perturbation
    /// `seed` so successive attempts differ even within the same nanosecond.
    /// NOT cryptographically secure — acceptable only for short-lived,
    /// one-time pairing codes (5-min TTL, single use). Do not reuse elsewhere.
    fn gen_code(seed: u32) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        format!("{:06}", nanos.wrapping_add(seed) % 1_000_000)
    }
}

impl IdentityResolver for SqliteIdentityStore {
    fn resolve(&self, channel_ref: &str, sender: &str, is_master: bool) -> Option<String> {
        let conn = self.conn.lock();
        if is_master {
            return Self::is_whitelisted(&conn, sender).then(|| sender.to_string());
        }
        let master_id: Option<String> = conn
            .query_row(
                "SELECT master_id FROM identity_mapping WHERE channel_ref = ?1 AND sender = ?2",
                params![channel_ref, sender],
                |row| row.get(0),
            )
            .ok();
        let master_id = master_id?;
        Self::is_whitelisted(&conn, &master_id).then_some(master_id)
    }

    fn issue_code(&self, master_id: &str) -> Option<String> {
        // Lock ordering note: we briefly take the `conn` lock here and release
        // it before taking the `codes` lock below. `redeem_code` takes `codes`
        // (temporary guard) and releases it before taking `conn`. Neither path
        // holds both locks at once, so there is no lock-ordering deadlock.
        {
            let conn = self.conn.lock();
            if !Self::is_whitelisted(&conn, master_id) {
                return None;
            }
        }
        // Generate + check + insert atomically under the codes lock (no TOCTOU).
        let mut codes = self.codes.lock();
        for seed in 0..16 {
            let code = Self::gen_code(seed);
            if !codes.contains_key(&code) {
                codes.insert(code.clone(), (master_id.to_string(), SystemTime::now()));
                return Some(code);
            }
        }
        None
    }

    fn redeem_code(&self, code: &str, channel_ref: &str, sender: &str) -> Result<String, String> {
        let entry = self.codes.lock().remove(code);
        let (master_id, issued_at) = entry.ok_or_else(|| "绑定码无效或已被使用".to_string())?;
        if issued_at.elapsed().map(|e| e > BIND_CODE_TTL).unwrap_or(true) {
            return Err("绑定码已过期,请重新获取".to_string());
        }
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO identity_mapping (channel_ref, sender, master_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(channel_ref, sender) DO UPDATE SET master_id = excluded.master_id",
            params![channel_ref, sender, master_id],
        )
        .map_err(|e| format!("写入映射失败：{e}"))?;
        Ok(master_id)
    }

    fn unbind(&self, channel_ref: &str, sender: &str) -> bool {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM identity_mapping WHERE channel_ref = ?1 AND sender = ?2",
            params![channel_ref, sender],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, SqliteIdentityStore) {
        let tmp = TempDir::new().unwrap();
        let store = SqliteIdentityStore::new(tmp.path()).unwrap();
        (tmp, store)
    }

    #[test]
    fn master_message_resolves_to_self_when_whitelisted() {
        let (_t, s) = store();
        s.seed_superusers(&["u_alice".to_string()]).unwrap();
        assert_eq!(
            s.resolve("dawnim.work", "u_alice", true),
            Some("u_alice".to_string())
        );
        // Non-whitelisted master user does not get a unified session.
        assert_eq!(s.resolve("dawnim.work", "u_stranger", true), None);
    }

    #[test]
    fn slave_message_resolves_via_binding() {
        let (_t, s) = store();
        s.seed_superusers(&["u_alice".to_string()]).unwrap();
        // No binding yet.
        assert_eq!(s.resolve("lark.work", "ou_aaa", false), None);
        // Pair and bind.
        let code = s.issue_code("u_alice").unwrap();
        let master = s.redeem_code(&code, "lark.work", "ou_aaa").unwrap();
        assert_eq!(master, "u_alice");
        // Now the slave message resolves.
        assert_eq!(
            s.resolve("lark.work", "ou_aaa", false),
            Some("u_alice".to_string())
        );
    }

    #[test]
    fn issue_code_rejects_non_superuser() {
        let (_t, s) = store();
        assert!(s.issue_code("u_nobody").is_none());
    }

    #[test]
    fn redeem_code_is_one_time() {
        let (_t, s) = store();
        s.seed_superusers(&["u_alice".to_string()]).unwrap();
        let code = s.issue_code("u_alice").unwrap();
        assert!(s.redeem_code(&code, "lark.work", "ou_aaa").is_ok());
        assert!(s.redeem_code(&code, "lark.work", "ou_bbb").is_err());
    }

    #[test]
    fn redeem_unknown_code_errors() {
        let (_t, s) = store();
        assert!(s.redeem_code("000000", "lark.work", "ou_aaa").is_err());
    }

    #[test]
    fn unbind_removes_mapping() {
        let (_t, s) = store();
        s.seed_superusers(&["u_alice".to_string()]).unwrap();
        let code = s.issue_code("u_alice").unwrap();
        s.redeem_code(&code, "lark.work", "ou_aaa").unwrap();
        assert!(s.unbind("lark.work", "ou_aaa"));
        assert_eq!(s.resolve("lark.work", "ou_aaa", false), None);
        // Second unbind is a no-op.
        assert!(!s.unbind("lark.work", "ou_aaa"));
    }

    #[test]
    fn seed_superusers_is_idempotent() {
        let (_t, s) = store();
        s.seed_superusers(&["u_alice".to_string()]).unwrap();
        s.seed_superusers(&["u_alice".to_string(), "u_bob".to_string()]).unwrap();
        assert_eq!(s.resolve("dawnim.work", "u_bob", true), Some("u_bob".to_string()));
    }
}
