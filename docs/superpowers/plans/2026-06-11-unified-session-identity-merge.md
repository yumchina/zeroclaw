# 统一会话（跨端身份合并）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让同一个人（master channel 的用户）在多个已配置子渠道的 1:1 私聊会话历史，合并到一个统一会话。

**Architecture:** 基础能力 / session 归一层。不新增 Channel 实体。新增独立 `identity.db`（映射表 + 白名单），在 orchestrator 计算 `session_key` 的入口处用 `resolve_session_key` 包装 `conversation_history_key`：master 渠道消息的 `sender` 即统一身份，从渠道消息经 `/bind` 建立的映射归并到同一 key。子渠道收发不变，回复天然回到来源渠道。

**Tech Stack:** Rust，rusqlite（bundled），parking_lot，chrono，std::time。改动集中在 `zeroclaw-infra`（新模块）、`zeroclaw-config`（config 字段）、`zeroclaw-channels::orchestrator`（接线 + 命令）。

**Spec:** `docs/superpowers/specs/2026-06-10-unified-session-identity-merge-design.md`

---

## 文件结构

- **新建** `crates/zeroclaw-infra/src/identity_store.rs` — `IdentityResolver` trait + `SqliteIdentityStore`（映射/白名单/配对码）。单一职责：身份解析与绑定。
- **修改** `crates/zeroclaw-infra/src/lib.rs` — 注册模块、`make_identity_store` 工厂。
- **修改** `crates/zeroclaw-config/src/schema.rs` — `ChannelsConfig` 增 `master_channel` / `superusers` 字段及 Default。
- **修改** `crates/zeroclaw-channels/src/orchestrator/mod.rs` — `IdentityRuntime` 类型、`ChannelRuntimeContext.identity` 字段、`resolve_session_key`、`/bind` `/unbind` 命令、启动注入、群聊判断扩展。

---

## Task 1: identity_store 模块（trait + SQLite 实现）

**Files:**
- Create: `crates/zeroclaw-infra/src/identity_store.rs`
- Modify: `crates/zeroclaw-infra/src/lib.rs:5-11`（模块声明区）

- [ ] **Step 1: 注册新模块**

在 `crates/zeroclaw-infra/src/lib.rs` 的模块声明区（第 5–11 行那组 `pub mod`）按字母序加入：

```rust
pub mod identity_store;
```

- [ ] **Step 2: 写失败测试（建文件并放测试）**

创建 `crates/zeroclaw-infra/src/identity_store.rs`，先只放类型骨架与测试：

```rust
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
```

- [ ] **Step 3: 运行测试，确认编译失败**

Run: `cargo test -p zeroclaw-infra identity_store`
Expected: FAIL — `SqliteIdentityStore::new` / `seed_superusers` / `resolve` 等未实现。

- [ ] **Step 4: 实现 `SqliteIdentityStore`**

在 `identity_store.rs` 的 `#[cfg(test)]` 之前插入实现：

```rust
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

    /// 6-digit code derived from the current nanosecond clock. Retries on the
    /// (extremely rare) in-flight collision. No `rand` dependency.
    fn gen_code(&self) -> String {
        let codes = self.codes.lock();
        for _ in 0..8 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let code = format!("{:06}", nanos % 1_000_000);
            if !codes.contains_key(&code) {
                return code;
            }
        }
        // Fallback: append a disambiguating suffix.
        format!("{:06}", codes.len() % 1_000_000)
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
        {
            let conn = self.conn.lock();
            if !Self::is_whitelisted(&conn, master_id) {
                return None;
            }
        }
        let code = self.gen_code();
        self.codes
            .lock()
            .insert(code.clone(), (master_id.to_string(), SystemTime::now()));
        Some(code)
    }

    fn redeem_code(&self, code: &str, channel_ref: &str, sender: &str) -> Result<String, String> {
        let entry = self.codes.lock().remove(code);
        let (master_id, issued_at) = entry.ok_or_else(|| "绑定码无效或已被使用".to_string())?;
        if issued_at.elapsed().map(|e| e > BIND_CODE_TTL).unwrap_or(true) {
            return Err("绑定码已过期，请重新获取".to_string());
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
```

- [ ] **Step 5: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-infra identity_store`
Expected: PASS（7 个测试全过）。

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-infra/src/identity_store.rs crates/zeroclaw-infra/src/lib.rs
git commit -m "feat(infra): add identity_store for cross-channel unified sessions"
```

---

## Task 2: make_identity_store 工厂

**Files:**
- Modify: `crates/zeroclaw-infra/src/lib.rs`（在 `make_session_backend` 之后）

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-infra/src/lib.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[test]
    fn make_identity_store_seeds_and_resolves() {
        use crate::identity_store::IdentityResolver;
        let tmp = TempDir::new().unwrap();
        let store = make_identity_store(tmp.path(), &["u_alice".to_string()]).unwrap();
        assert_eq!(
            store.resolve("dawnim.work", "u_alice", true),
            Some("u_alice".to_string())
        );
        let db = tmp.path().join("sessions").join("identity.db");
        assert!(db.exists(), "identity.db must be created under sessions/");
    }
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p zeroclaw-infra make_identity_store_seeds_and_resolves`
Expected: FAIL — `make_identity_store` 未定义。

- [ ] **Step 3: 实现工厂**

在 `lib.rs` 中 `make_session_backend` 函数之后插入：

```rust
/// Construct the cross-channel identity store and seed the superuser whitelist.
///
/// Opens `{workspace}/sessions/identity.db` and inserts each `superusers`
/// entry into the `unified_member` whitelist (idempotent). Call only when
/// `[channels].master_channel` is configured.
pub fn make_identity_store(
    workspace_dir: &Path,
    superusers: &[String],
) -> std::io::Result<Arc<dyn identity_store::IdentityResolver>> {
    let store = identity_store::SqliteIdentityStore::new(workspace_dir)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    store
        .seed_superusers(superusers)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(Arc::new(store))
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p zeroclaw-infra`
Expected: PASS（含新测试）。

- [ ] **Step 5: 提交**

```bash
git add crates/zeroclaw-infra/src/lib.rs
git commit -m "feat(infra): add make_identity_store factory with superuser seeding"
```

---

## Task 3: config 增加 master_channel / superusers

**Files:**
- Modify: `crates/zeroclaw-config/src/schema.rs:10773`（`ChannelsConfig` 字段区，紧接 `pub cli` 之后）
- Modify: `crates/zeroclaw-config/src/schema.rs:11318`（`ChannelsConfig` 的 `Default` impl）

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-config/src/schema.rs` 文件末尾的 `#[cfg(test)] mod tests`（若无则新建一个 `mod unified_cfg_tests`）中加入：

```rust
#[cfg(test)]
mod unified_cfg_tests {
    use super::ChannelsConfig;

    #[test]
    fn channels_config_parses_master_and_superusers() {
        let toml = r#"
            master_channel = "dawnim.work"
            superusers = ["u_alice", "u_bob"]
        "#;
        let cfg: ChannelsConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.master_channel.as_deref(), Some("dawnim.work"));
        assert_eq!(cfg.superusers, vec!["u_alice".to_string(), "u_bob".to_string()]);
    }

    #[test]
    fn channels_config_defaults_are_empty() {
        let cfg: ChannelsConfig = toml::from_str("").unwrap();
        assert!(cfg.master_channel.is_none());
        assert!(cfg.superusers.is_empty());
    }
}
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p zeroclaw-config channels_config_parses_master_and_superusers`
Expected: FAIL — 字段不存在。

- [ ] **Step 3: 加字段**

在 `schema.rs:10776`（`pub cli: bool,` 之后）插入：

```rust
    /// Master channel ChannelRef (`"<type>.<alias>"`, e.g. `"dawnim.work"`).
    /// When set, unified cross-channel sessions are enabled: the master
    /// channel's user id IS the unified person id. `None` disables the feature.
    #[serde(default)]
    pub master_channel: Option<String>,
    /// Master-channel user ids seeded into the unified-session whitelist on
    /// first init. Only these users may initiate `/bind`.
    #[serde(default)]
    pub superusers: Vec<String>,
```

- [ ] **Step 4: 加 Default 初始化**

在 `schema.rs:11318` 区域的 `ChannelsConfig` Default 构造块中（与 `session_persistence: true,` 同级）加入：

```rust
            master_channel: None,
            superusers: Vec::new(),
```

- [ ] **Step 5: 运行，确认通过**

Run: `cargo test -p zeroclaw-config`
Expected: PASS（含新测试；其余 config 测试不回归）。

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-config/src/schema.rs
git commit -m "feat(config): add channels.master_channel and channels.superusers"
```

---

## Task 4: ChannelRuntimeContext 增加 identity 字段

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（类型定义 + 全部 `ChannelRuntimeContext { .. }` 字面量）

- [ ] **Step 1: 定义 `IdentityRuntime` 类型**

在 `mod.rs` 的 `ChannelRuntimeContext` 定义（第 357 行 `#[derive(Clone)] struct ChannelRuntimeContext`）之前插入：

```rust
/// Runtime handle for unified cross-channel sessions. Present only when
/// `[channels].master_channel` is configured.
#[derive(Clone)]
struct IdentityRuntime {
    resolver: Arc<dyn zeroclaw_infra::identity_store::IdentityResolver>,
    /// `"<type>.<alias>"` of the master channel.
    master_channel: String,
}
```

- [ ] **Step 2: 加字段到 struct**

在 `ChannelRuntimeContext` 的 `session_store` 字段（第 406 行）之后加入：

```rust
    /// Cross-channel identity resolver + master channel ref. `None` disables
    /// unified sessions (behaviour identical to before this feature).
    identity: Option<Arc<IdentityRuntime>>,
```

- [ ] **Step 3: 编译，让编译器列出所有缺字段的字面量**

Run: `cargo build -p zeroclaw-channels`
Expected: FAIL — 每个 `ChannelRuntimeContext { .. }` 字面量报 `missing field identity`。

- [ ] **Step 4: 在每个字面量补 `identity: None`**

对编译器报告的**每一处** `ChannelRuntimeContext { .. }`（生产路径 `mod.rs:8634`、`mod.rs:9456`、`mod.rs:9552`，以及全部 `#[cfg(test)]` 字面量），在 `session_store: ...,` 一行旁加：

```rust
            identity: None,
```

生产路径 `mod.rs:8634` 暂时也填 `None`——Task 7 会改成实际注入。逐处补齐直到 `cargo build -p zeroclaw-channels` 通过。

- [ ] **Step 5: 编译通过**

Run: `cargo build -p zeroclaw-channels`
Expected: 成功（无 `missing field`）。

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): add identity field to ChannelRuntimeContext"
```

---

## Task 5: resolve_session_key 与历史路径注入

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（新增函数 + 第 3667 行、第 2167 行注入 + 第 3688 行 sender_id）

- [ ] **Step 1: 写失败测试**

在 `mod.rs` 测试区（与现有 `conversation_history_key` 测试同一 `mod tests`）加入。先准备一个内存 resolver stub 与构造 helper：

```rust
    struct StubResolver {
        // (channel_ref, sender) -> master_id
        map: std::collections::HashMap<(String, String), String>,
        whitelist: std::collections::HashSet<String>,
    }
    impl zeroclaw_infra::identity_store::IdentityResolver for StubResolver {
        fn resolve(&self, channel_ref: &str, sender: &str, is_master: bool) -> Option<String> {
            if is_master {
                return self.whitelist.contains(sender).then(|| sender.to_string());
            }
            let m = self.map.get(&(channel_ref.to_string(), sender.to_string()))?;
            self.whitelist.contains(m).then(|| m.clone())
        }
        fn issue_code(&self, _m: &str) -> Option<String> { None }
        fn redeem_code(&self, _c: &str, _ch: &str, _s: &str) -> Result<String, String> {
            Err("stub".into())
        }
        fn unbind(&self, _ch: &str, _s: &str) -> bool { false }
    }

    fn dm(channel: &str, alias: &str, sender: &str) -> zeroclaw_api::channel::ChannelMessage {
        let mut m = zeroclaw_api::channel::ChannelMessage::new(
            "id1", sender, sender, "hi", channel, 0,
        );
        m.channel_alias = Some(alias.to_string());
        m
    }

    #[test]
    fn resolve_session_key_master_uses_sender_identity() {
        let mut whitelist = std::collections::HashSet::new();
        whitelist.insert("u_alice".to_string());
        let ident = IdentityRuntime {
            resolver: Arc::new(StubResolver { map: Default::default(), whitelist }),
            master_channel: "dawnim.work".to_string(),
        };
        let msg = dm("dawnim", "work", "u_alice");
        assert_eq!(resolve_session_key(&msg, Some(&ident)), "unified_u_alice");
    }

    #[test]
    fn resolve_session_key_slave_uses_binding() {
        let mut map = std::collections::HashMap::new();
        map.insert(("lark.work".to_string(), "ou_aaa".to_string()), "u_alice".to_string());
        let mut whitelist = std::collections::HashSet::new();
        whitelist.insert("u_alice".to_string());
        let ident = IdentityRuntime {
            resolver: Arc::new(StubResolver { map, whitelist }),
            master_channel: "dawnim.work".to_string(),
        };
        let msg = dm("lark", "work", "ou_aaa");
        assert_eq!(resolve_session_key(&msg, Some(&ident)), "unified_u_alice");
    }

    #[test]
    fn resolve_session_key_unbound_falls_back_to_base() {
        let ident = IdentityRuntime {
            resolver: Arc::new(StubResolver { map: Default::default(), whitelist: Default::default() }),
            master_channel: "dawnim.work".to_string(),
        };
        let msg = dm("lark", "work", "ou_stranger");
        assert_eq!(resolve_session_key(&msg, Some(&ident)), conversation_history_key(&msg));
    }

    #[test]
    fn resolve_session_key_none_identity_is_base() {
        let msg = dm("lark", "work", "ou_aaa");
        assert_eq!(resolve_session_key(&msg, None), conversation_history_key(&msg));
    }

    #[test]
    fn resolve_session_key_group_is_not_unified() {
        let mut whitelist = std::collections::HashSet::new();
        whitelist.insert("u_alice".to_string());
        let ident = IdentityRuntime {
            resolver: Arc::new(StubResolver { map: Default::default(), whitelist }),
            master_channel: "dawnim.work".to_string(),
        };
        let mut msg = dm("dawnim", "work", "u_alice");
        msg.reply_target = "group:team".to_string(); // group → no unify
        assert_eq!(resolve_session_key(&msg, Some(&ident)), conversation_history_key(&msg));
    }
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p zeroclaw-channels resolve_session_key`
Expected: FAIL — `resolve_session_key` 未定义。

- [ ] **Step 3: 实现 `resolve_session_key`**

在 `conversation_history_key` 函数（第 501 行 `}` 结束）之后插入：

```rust
/// Wrap `conversation_history_key` with cross-channel identity merging.
/// Returns `unified_<master_id>` for 1:1 messages whose (channel_ref, sender)
/// resolves to a whitelisted master id; otherwise the base key (unchanged
/// per-channel isolation). Group chats and unconfigured identity always use
/// the base key.
fn resolve_session_key(
    msg: &zeroclaw_api::channel::ChannelMessage,
    identity: Option<&IdentityRuntime>,
) -> String {
    let base = conversation_history_key(msg);
    let Some(identity) = identity else {
        return base;
    };
    if is_group_reply_target(&msg.reply_target) {
        return base;
    }
    let channel_ref = match &msg.channel_alias {
        Some(alias) => format!("{}.{}", msg.channel, alias),
        None => msg.channel.clone(),
    };
    let is_master = channel_ref == identity.master_channel;
    match identity.resolver.resolve(&channel_ref, &msg.sender, is_master) {
        Some(master_id) => sanitize_session_key(&format!("unified_{master_id}")),
        None => base,
    }
}
```

- [ ] **Step 4: 运行，确认通过**

Run: `cargo test -p zeroclaw-channels resolve_session_key`
Expected: PASS（5 个测试全过）。

- [ ] **Step 5: 注入历史路径（第 3667 行）**

将 `mod.rs:3667`：

```rust
    let history_key = conversation_history_key(&msg);
```

改为：

```rust
    let history_key = resolve_session_key(&msg, ctx.identity.as_deref());
```

- [ ] **Step 6: 合并会话的 sender_id 填 master_id（第 3688 行）**

将 `mod.rs:3688` 的 `sender_id` 行：

```rust
            sender_id: Some(msg.sender.as_str()).filter(|s| !s.is_empty()),
```

改为（统一会话用 master_id，dashboard 语义清晰）：

```rust
            sender_id: history_key
                .strip_prefix("unified_")
                .or(Some(msg.sender.as_str()))
                .filter(|s| !s.is_empty()),
```

- [ ] **Step 7: 命令路径 sender_key 也归一（第 2167 行）**

将 `mod.rs:2167`：

```rust
    let sender_key = conversation_history_key(msg);
```

改为（使 `/new` 能正确清除统一会话历史、route 选择跨端共享）：

```rust
    let sender_key = resolve_session_key(msg, ctx.identity.as_deref());
```

- [ ] **Step 8: 全量编译 + 测试**

Run: `cargo test -p zeroclaw-channels`
Expected: PASS（无回归）。

- [ ] **Step 9: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): unify session_key via resolve_session_key on history paths"
```

---

## Task 6: /bind 与 /unbind 命令

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（enum 第 296 行、parse 第 1056 行、handle 第 2252 行后）

- [ ] **Step 1: 写失败测试（parse）**

在 `mod.rs` 测试区加入：

```rust
    #[test]
    fn parse_runtime_command_recognizes_bind_and_unbind() {
        assert!(matches!(
            parse_runtime_command("lark", "/bind 123456"),
            Some(ChannelRuntimeCommand::Bind(Some(c))) if c == "123456"
        ));
        assert!(matches!(
            parse_runtime_command("dawnim", "/bind"),
            Some(ChannelRuntimeCommand::Bind(None))
        ));
        assert!(matches!(
            parse_runtime_command("lark", "/unbind"),
            Some(ChannelRuntimeCommand::Unbind)
        ));
    }
```

- [ ] **Step 2: 运行，确认失败**

Run: `cargo test -p zeroclaw-channels parse_runtime_command_recognizes_bind_and_unbind`
Expected: FAIL — 变体不存在。

- [ ] **Step 3: 加 enum 变体**

在 `ChannelRuntimeCommand`（第 296 行）的 `NewSession,` 之后加：

```rust
    /// `/bind` — no arg on master channel issues a code; `<code>` on a slave
    /// channel redeems it.
    Bind(Option<String>),
    /// `/unbind` — remove the current slave-channel binding.
    Unbind,
```

- [ ] **Step 4: 加 parse 分支**

在 `parse_runtime_command`（第 1058 行 `"/new" => ...` 之后）加：

```rust
        "/bind" => Some(ChannelRuntimeCommand::Bind(
            parts.next().map(|s| s.trim().to_string()),
        )),
        "/unbind" => Some(ChannelRuntimeCommand::Unbind),
```

- [ ] **Step 5: 运行 parse 测试，确认通过**

Run: `cargo test -p zeroclaw-channels parse_runtime_command_recognizes_bind_and_unbind`
Expected: PASS。

- [ ] **Step 6: 加 handle 分支**

在 `handle_runtime_command_if_needed` 的 `match command { .. }` 中，`ChannelRuntimeCommand::NewSession => { .. }`（第 2252–2269 行）分支**之后**加入以下两个 arm（每个 arm 整体求值为 `String`，与现有 `let response = match command { .. }` 一致）：

```rust
        ChannelRuntimeCommand::Bind(arg) => match ctx.identity.as_deref() {
            None => "统一会话未启用。".to_string(),
            Some(identity) => {
                let channel_ref = match &msg.channel_alias {
                    Some(alias) => format!("{}.{}", msg.channel, alias),
                    None => msg.channel.clone(),
                };
                let is_master = channel_ref == identity.master_channel;
                match arg {
                    None if is_master => match identity.resolver.issue_code(&msg.sender) {
                        Some(code) => format!(
                            "绑定码：{code}\n请在 5 分钟内，到其他渠道发送 /bind {code} 完成绑定。"
                        ),
                        None => "你不是 superuser，无法发起绑定。".to_string(),
                    },
                    None => "请先在主渠道发送 /bind 获取绑定码，再在此渠道发送 /bind <码>。".to_string(),
                    Some(_) if is_master => "主渠道无需绑定。".to_string(),
                    Some(code) => match identity.resolver.redeem_code(&code, &channel_ref, &msg.sender) {
                        Ok(master_id) => format!("已绑定到 {master_id}，此后本渠道会话将与主渠道合并。"),
                        Err(reason) => format!("绑定失败：{reason}"),
                    },
                }
            }
        },
        ChannelRuntimeCommand::Unbind => match ctx.identity.as_deref() {
            None => "统一会话未启用。".to_string(),
            Some(identity) => {
                let channel_ref = match &msg.channel_alias {
                    Some(alias) => format!("{}.{}", msg.channel, alias),
                    None => msg.channel.clone(),
                };
                if identity.resolver.unbind(&channel_ref, &msg.sender) {
                    "已解绑，本渠道会话恢复独立。".to_string()
                } else {
                    "当前没有绑定。".to_string()
                }
            }
        },
```

- [ ] **Step 7: 全量编译 + 测试**

Run: `cargo test -p zeroclaw-channels`
Expected: PASS。

- [ ] **Step 8: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): add /bind and /unbind paired identity commands"
```

---

## Task 7: 启动构建 identity store 并注入 ctx

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs:8010` 后（构建 identity runtime）与 `:8704` 区域（注入字面量）

- [ ] **Step 1: 构建 IdentityRuntime（在 shared_session_store 之后）**

在 `mod.rs:8010`（`shared_session_store` 的 `let ... = if ... { .. } else { None };` 结束）之后插入：

```rust
    // Cross-channel identity runtime (unified sessions). Built once and shared
    // across agent ctxs, like the session backend. Disabled unless
    // `[channels].master_channel` is set.
    let shared_identity: Option<Arc<IdentityRuntime>> = match config
        .channels
        .master_channel
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        Some(master_channel) => {
            match zeroclaw_infra::make_identity_store(&config.data_dir, &config.channels.superusers) {
                Ok(resolver) => {
                    ::zeroclaw_log::record!(
                        INFO,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
                        &format!("🔗 Unified sessions enabled (master: {master_channel})")
                    );
                    Some(Arc::new(IdentityRuntime {
                        resolver,
                        master_channel: master_channel.to_string(),
                    }))
                }
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "Unified sessions disabled (identity store init failed)"
                    );
                    None
                }
            }
        }
        None => None,
    };
```

- [ ] **Step 2: 注入到生产 ctx 字面量**

将 `mod.rs:8704` 的 `session_store: shared_session_store.clone(),` 一行之后（即 Task 4 在此处填的 `identity: None,`）改为：

```rust
            identity: shared_identity.clone(),
```

- [ ] **Step 3: 编译**

Run: `cargo build -p zeroclaw-channels`
Expected: 成功。

- [ ] **Step 4: 集成测试（端到端归一）**

在 `mod.rs` 测试区加入一个端到端测试，验证 master + 绑定后的 slave 命中同一 `session_key`：

```rust
    #[test]
    fn end_to_end_master_and_bound_slave_share_session_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let resolver = zeroclaw_infra::make_identity_store(tmp.path(), &["u_alice".to_string()])
            .unwrap();
        // Bind lark.work/ou_aaa -> u_alice via a real code.
        let code = resolver.issue_code("u_alice").unwrap();
        resolver.redeem_code(&code, "lark.work", "ou_aaa").unwrap();

        let ident = IdentityRuntime { resolver, master_channel: "dawnim.work".to_string() };

        let master_msg = dm("dawnim", "work", "u_alice");
        let slave_msg = dm("lark", "work", "ou_aaa");
        let stranger = dm("lark", "work", "ou_zzz");

        assert_eq!(
            resolve_session_key(&master_msg, Some(&ident)),
            resolve_session_key(&slave_msg, Some(&ident)),
            "master and bound slave must share the unified session_key"
        );
        assert_eq!(resolve_session_key(&master_msg, Some(&ident)), "unified_u_alice");
        assert_eq!(
            resolve_session_key(&stranger, Some(&ident)),
            conversation_history_key(&stranger),
            "unbound stranger stays isolated"
        );
    }
```

- [ ] **Step 5: 运行，确认通过**

Run: `cargo test -p zeroclaw-channels end_to_end_master_and_bound_slave_share_session_key`
Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): build and inject identity runtime at startup"
```

---

## Task 8: 扩展群聊判断以覆盖参与渠道

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs:2411`（`is_group_reply_target`）

**背景：** `is_group_reply_target` 当前只认 `@g.us` / `group:` 前缀（另有 `wecom_ws` 的 `group--` 在别处处理）。统一会话仅限 1:1 私聊，需确认 dawnim / lark / qq / wechat / wecom 的群聊 `reply_target` 不会被误判为私聊而并入统一会话。

- [ ] **Step 1: 调查各参与渠道的 reply_target 构造**

Run（逐个查看每个渠道如何设置入站 `ChannelMessage.reply_target`，重点找群聊分支）：

```bash
rg -n "reply_target" crates/zeroclaw-channels/src/dawn_im crates/zeroclaw-channels/src/lark.rs crates/zeroclaw-channels/src/qq.rs crates/zeroclaw-channels/src/wecom_ws.rs
rg -n "reply_target" src/channels/wechat.rs
```

记录每个渠道**群聊**场景下 `reply_target` 的实际形态（前缀/格式）。

- [ ] **Step 2: 写失败测试（用 Step 1 发现的群聊格式）**

在 `mod.rs` 测试区加入断言。下面给出已知前缀的测试；**把 Step 1 发现的每个新群聊格式各加一行 `assert!(is_group_reply_target("<发现的格式>"))`**：

```rust
    #[test]
    fn is_group_reply_target_covers_participating_channels() {
        // Existing coverage (must keep passing).
        assert!(is_group_reply_target("123@g.us"));
        assert!(is_group_reply_target("group:team"));
        // Newly added prefixes discovered in Step 1, e.g. wecom group rooms:
        assert!(is_group_reply_target("group--room-1"));
        // 1:1 private chats must remain non-group:
        assert!(!is_group_reply_target("ou_alice"));
        assert!(!is_group_reply_target("u_alice"));
    }
```

- [ ] **Step 3: 运行，确认失败**

Run: `cargo test -p zeroclaw-channels is_group_reply_target_covers_participating_channels`
Expected: FAIL — `group--room-1`（及 Step 1 新增格式）未被识别为群聊。

- [ ] **Step 4: 扩展判断函数**

将 `mod.rs:2411` 的 `is_group_reply_target` 扩展为（按 Step 1 发现补充 `||` 条件，下面纳入 `group--`）：

```rust
fn is_group_reply_target(reply_target: &str) -> bool {
    reply_target.contains("@g.us")
        || reply_target.starts_with("group:")
        || reply_target.starts_with("group--")
    // + Step 1 中发现的其它群聊前缀/标志，按需追加 `|| ...`
}
```

- [ ] **Step 5: 运行，确认通过**

Run: `cargo test -p zeroclaw-channels is_group_reply_target`
Expected: PASS。

- [ ] **Step 6: 全量回归**

Run: `cargo test -p zeroclaw-channels`
Expected: PASS（确认 `wecom_ws` 既有 `group--` 相关测试未受影响）。

- [ ] **Step 7: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): extend group-chat detection for unified-session scope"
```

---

## 收尾验证

- [ ] **全量构建 + 测试**

Run: `cargo test --workspace`
Expected: PASS。

- [ ] **手动冒烟（可选，需真实渠道）**

1. config 配置 `[channels] master_channel = "dawnim.<alias>"` 与 `superusers = ["<你的dawnim_id>"]`。
2. 在 dawnim 私聊发消息 → 在从渠道（lark）发 `/bind` → 提示去主渠道取码。
3. 在 dawnim 发 `/bind` → 得到码 → 在 lark 发 `/bind <码>` → 提示绑定成功。
4. 在 lark 提一个只在 dawnim 聊过的话题 → agent 能接上上下文。

---

## 实现说明（YAGNI / 边界）

- **不**迁移存量历史：仅新消息归一（spec §3）。
- **不**改 `conversation_history_key` 纯函数及其测试。
- identity 未配置时 `ctx.identity = None`，所有路径回退 base key，与现状一致。
- 配对码仅存内存，daemon 重启丢失属预期（短期一次性，重发即可）。
- memory 层（`conversation_memory_key` / `sender_memory_session_ids`）本期不动。
