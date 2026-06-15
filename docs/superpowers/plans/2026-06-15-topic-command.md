# `/topic` 跨渠道话题绑定 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 superuser 在非 DawnIM 渠道（feishu / wecom_ws 等）添加 `/topic` 命令，把 (channel, sender) 绑定到 DawnIM master 上的某个 topic，使该渠道消息自动归入 `unified_<master>_<topic>` 会话/记忆 namespace。

**Architecture:** 新增进程级 `TopicBindingRegistry`（内存 HashMap + JSON 落盘，与 `IdentityRuntime` 并列）；新增 `resolve_effective_topic(msg, channel_ref, master, binding)` 纯函数计算逻辑 topic（master 渠道直接用 msg.thread_ts，slave 渠道 thread_ts 优先、binding 兜底）；orchestrator 在 `process_channel_message_body` 中调用一次，结果下发到 `resolve_session_key` 和 `ChannelOrigin.topic`；`ChannelRuntimeCommand` 新增 `Topic(TopicAction)` variant，handler 复用 `IdentityResolver::resolve` 做 superuser 鉴权，topic 列表通过新增的 `SqliteMemory::list_unified_topics` 查询 `dawn_im_<alias>` 库。

**Tech Stack:** Rust / SQLite (rusqlite) / serde_json / parking_lot::RwLock / tempfile (tests) / async-trait (channel API)

**Spec:** [docs/superpowers/specs/2026-06-15-topic-command-design.md](../specs/2026-06-15-topic-command-design.md)

---

## File Structure

| 文件 | 职责 |
|------|------|
| `crates/zeroclaw-infra/src/topic_binding.rs` (新) | `TopicBindingRegistry` 数据结构、JSON 序列化、并发安全的 get/set/clear |
| `crates/zeroclaw-infra/src/lib.rs` (改) | 注册新模块 + `make_topic_binding_registry` 工厂 |
| `crates/zeroclaw-memory/src/sqlite.rs` (改) | 新增 `SqliteMemory::list_unified_topics(master_id)` 查询方法 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` (改) | `ChannelRuntimeCommand::Topic` + `TopicAction` enum + parse 扩展 + handler 实现 + `resolve_effective_topic` 引入 + `ChannelRuntimeContext.topic_binding` 字段 + 启动期装配 + `process_channel_message_body` 集成 |

---

## Conventions

- **Commit message**：Conventional Commits `<type>(<scope>): <message>`
- **不**添加 `Co-Authored-By: Claude` 或任何 AI 属性 trailer
- **测试运行**：`cargo test -p <crate> <test_name> -- --nocapture` (单测) ／ `cargo test -p zeroclaw-channels` (crate 全测)
- **rustfmt**：每个 commit 前 `cargo fmt -p <crate>`，仅格式化本次改动的 crate
- **clippy**：每个 commit 前 `cargo clippy -p <crate> --all-targets -- -D warnings`
- **scope 严格控制**：本次改动仅触及上表 4 个文件；如发现需要改其他文件，停下来汇报

---

## Task 1: `TopicBindingRegistry` 内存基础结构与并发 API

**Files:**
- Create: `crates/zeroclaw-infra/src/topic_binding.rs`

- [ ] **Step 1: 写失败的单元测试**

新建 `crates/zeroclaw-infra/src/topic_binding.rs`，先只放数据结构骨架与测试：

```rust
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
```

注册模块（添加一行）到 `crates/zeroclaw-infra/src/lib.rs` 的 `pub mod ...` 块（紧邻 `identity_store`）：

```rust
pub mod topic_binding;
```

- [ ] **Step 2: 运行测试确认失败/通过**

Run:
```
cargo test -p zeroclaw-infra topic_binding --
```
Expected: 6 个测试全部 PASS（结构与方法已实现）

- [ ] **Step 3: cargo fmt + clippy**

Run:
```
cargo fmt -p zeroclaw-infra
cargo clippy -p zeroclaw-infra --all-targets -- -D warnings
```
Expected: no diffs / no warnings

- [ ] **Step 4: Commit**

```
git add crates/zeroclaw-infra/src/topic_binding.rs crates/zeroclaw-infra/src/lib.rs
git commit -m "feat(infra): introduce TopicBindingRegistry in-memory store

Holds (channel_ref, sender) -> topic_id mappings. Persistence layer
lands in a follow-up; this commit ships the concurrent-safe API and
unit coverage for get/set/clear semantics."
```

---

## Task 2: `TopicBindingRegistry` 持久化 (JSON 落盘 + 启动加载)

**Files:**
- Modify: `crates/zeroclaw-infra/src/topic_binding.rs`

- [ ] **Step 1: 添加持久化测试 (先失败)**

在 `tests` 模块底部追加测试：

```rust
    use tempfile::TempDir;

    fn temp_path() -> (TempDir, PathBuf) {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sessions").join("topic_binding.json");
        (tmp, path)
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
        assert_eq!(r2.get("feishu.work", "u_alice"), Some("db_lock".to_string()));
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
```

注：`tempfile` 在 zeroclaw-infra 中是否已在 `[dev-dependencies]`？如果不在，需先加：

```toml
# crates/zeroclaw-infra/Cargo.toml [dev-dependencies]
tempfile = "3"
```

先检查：
```
grep -n tempfile crates/zeroclaw-infra/Cargo.toml
```
若无，按上述加入。

- [ ] **Step 2: 实现 `load` + 内部 `persist` 方法**

替换 Task 1 写的简单 `new`，并补充 IO：

```rust
use std::io;
use std::path::Path;
use std::collections::BTreeMap;  // BTree for deterministic JSON output

impl TopicBindingRegistry {
    /// Open the registry at `persist_path`. If the file is missing or
    /// unparseable, returns an empty registry (with a warning log for the
    /// parse-failure case). Best-effort: filesystem permissions are the
    /// only hard failure.
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
            // Split on the FIRST '|' only; entries with extra '|' are corrupt.
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

    /// Best-effort sync write. Failure is logged, not propagated, so a
    /// transient disk error never blocks the user's command. The next
    /// successful write restores consistency.
    fn persist_locked(&self, snapshot: &HashMap<BindingKey, String>) {
        if let Some(parent) = self.persist_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
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
```

替换 set/clear 末尾，触发持久化：

```rust
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
```

并删掉 Task 1 留下的简单 `new` 构造器（已被 `load` 替代）。如果其他测试调用了 `reg()`，更新它：

```rust
    fn reg() -> TopicBindingRegistry {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("sessions").join("topic_binding.json");
        // Leak the TempDir for the lifetime of the test — test is short and
        // the OS will reclaim. (Or: thread (TempDir, Registry) through tests.)
        let _ = Box::leak(Box::new(tmp));
        TopicBindingRegistry::load(path).unwrap()
    }
```

确保依赖（`serde_json`、`zeroclaw-log`、`tempfile`）在 `Cargo.toml` 中可见。`serde_json` 与 `zeroclaw-log` 通常 zeroclaw-infra 已用；检查后视情况追加。

```
grep -n "serde_json\|zeroclaw-log" crates/zeroclaw-infra/Cargo.toml
```

- [ ] **Step 3: 运行测试**

Run:
```
cargo test -p zeroclaw-infra topic_binding --
```
Expected: 11 个测试全部 PASS（6 旧 + 5 新）

- [ ] **Step 4: cargo fmt + clippy**

```
cargo fmt -p zeroclaw-infra
cargo clippy -p zeroclaw-infra --all-targets -- -D warnings
```

- [ ] **Step 5: Commit**

```
git add crates/zeroclaw-infra/src/topic_binding.rs crates/zeroclaw-infra/Cargo.toml
git commit -m "feat(infra): add JSON persistence to TopicBindingRegistry

Best-effort sync write to {data_dir}/sessions/topic_binding.json on
mutation; load on startup; corrupt/malformed entries downgrade to
empty + warning so a bad file never blocks startup."
```

---

## Task 3: `make_topic_binding_registry` 工厂函数

**Files:**
- Modify: `crates/zeroclaw-infra/src/lib.rs`

- [ ] **Step 1: 在 lib.rs 测试模块底部添加失败测试**

```rust
    #[test]
    fn make_topic_binding_registry_creates_under_sessions() {
        let tmp = TempDir::new().unwrap();
        let reg = make_topic_binding_registry(tmp.path()).unwrap();
        reg.set("feishu.work", "u_alice", "db_lock");
        let json_path = tmp
            .path()
            .join("sessions")
            .join("topic_binding.json");
        assert!(json_path.exists(), "topic_binding.json must persist under sessions/");
        // Round-trip via a fresh registry.
        let reg2 = make_topic_binding_registry(tmp.path()).unwrap();
        assert_eq!(
            reg2.get("feishu.work", "u_alice"),
            Some("db_lock".to_string())
        );
    }
```

- [ ] **Step 2: 实现工厂**

紧邻 `make_identity_store` 后插入：

```rust
/// Construct the cross-channel topic binding registry.
///
/// Opens (or creates) `{workspace}/sessions/topic_binding.json`. Call only
/// when `[channels].master_channel` is configured (same gate as
/// `make_identity_store`).
pub fn make_topic_binding_registry(
    workspace_dir: &Path,
) -> std::io::Result<Arc<topic_binding::TopicBindingRegistry>> {
    let path = workspace_dir
        .join("sessions")
        .join("topic_binding.json");
    let reg = topic_binding::TopicBindingRegistry::load(path)?;
    Ok(Arc::new(reg))
}
```

- [ ] **Step 3: 运行测试**

```
cargo test -p zeroclaw-infra make_topic_binding_registry --
```
Expected: PASS

跟所有 zeroclaw-infra 测试一起跑确认无回归：
```
cargo test -p zeroclaw-infra
```
Expected: 全部 PASS

- [ ] **Step 4: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-infra
cargo clippy -p zeroclaw-infra --all-targets -- -D warnings
git add crates/zeroclaw-infra/src/lib.rs
git commit -m "feat(infra): expose make_topic_binding_registry factory

Mirrors make_identity_store: workspace_dir -> Arc<TopicBindingRegistry>.
Persistence path is {workspace}/sessions/topic_binding.json."
```

---

## Task 4: `SqliteMemory::list_unified_topics` 查询

**Files:**
- Modify: `crates/zeroclaw-memory/src/sqlite.rs`

- [ ] **Step 1: 找到 `memory_entries` 表的 schema + session_id 列名**

确认列名。Run:
```
grep -n "memory_entries\|session_id" crates/zeroclaw-memory/src/sqlite.rs | head -20
```

记录正确列名 (预计 `session_id`)。若与本任务假设不同，更新下面 SQL 中的列名再继续。

- [ ] **Step 2: 写失败测试**

在 `sqlite.rs` 的 `#[cfg(test)] mod tests` 中追加。先找到现有测试 module 名（一般是 `tests`），插入：

```rust
    #[test]
    fn list_unified_topics_returns_distinct_topic_suffixes() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        // Seed memory entries with various session_ids
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            for (sid, content) in [
                ("unified_u_alice_db_lock", "msg1"),
                ("unified_u_alice_db_lock", "msg2"), // duplicate session_id
                ("unified_u_alice_migrations", "msg3"),
                ("unified_u_alice_casual", "msg4"),
                ("unified_u_alice", "msg5"),       // no topic suffix — should NOT appear
                ("unified_u_bob_other_topic", "msg6"), // different master_id
                ("dawnim_x_y_z", "msg7"),          // non-unified
            ] {
                mem.store(
                    "test_key",
                    content,
                    MemoryCategory::default(),
                    Some(sid.to_string()),
                )
                .await
                .unwrap();
            }
        });

        let mut topics = mem.list_unified_topics("u_alice").unwrap();
        topics.sort();
        assert_eq!(topics, vec!["casual", "db_lock", "migrations"]);
    }

    #[test]
    fn list_unified_topics_empty_when_no_match() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let topics = mem.list_unified_topics("u_nobody").unwrap();
        assert!(topics.is_empty());
    }

    #[test]
    fn list_unified_topics_does_not_leak_other_master_ids() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new("test", tmp.path()).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            mem.store(
                "k",
                "x",
                MemoryCategory::default(),
                Some("unified_u_alice_secret".to_string()),
            )
            .await
            .unwrap();
        });
        let topics = mem.list_unified_topics("u_bob").unwrap();
        assert!(topics.is_empty(), "u_bob must not see u_alice's topics");
    }
```

确认 imports 在 test module 顶部：`use super::*;`、`use tempfile::TempDir;`、`use crate::traits::MemoryCategory;`（若不在）；按需补齐。

- [ ] **Step 3: 实现方法**

在 `impl SqliteMemory { ... }` 块（任一公开 impl）内加：

```rust
    /// List distinct topic suffixes of session_ids matching
    /// `unified_<master_id>_<topic>`. Returns the topic part only.
    ///
    /// Used by the `/topic list` slash command to enumerate topics a
    /// superuser has accumulated on the master (DawnIM) channel.
    pub fn list_unified_topics(&self, master_id: &str) -> anyhow::Result<Vec<String>> {
        let prefix = format!("unified_{master_id}_");
        let pattern = format!("{prefix}%");
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT session_id
               FROM memory_entries
              WHERE session_id LIKE ?1",
        )?;
        let rows = stmt.query_map(params![pattern], |row| row.get::<_, String>(0))?;
        let mut topics = Vec::new();
        for row in rows {
            let sid = row?;
            if let Some(topic) = sid.strip_prefix(&prefix) {
                if !topic.is_empty() {
                    topics.push(topic.to_string());
                }
            }
        }
        Ok(topics)
    }
```

若列名实际不是 `session_id`，调整 SQL。

- [ ] **Step 4: 运行测试**

```
cargo test -p zeroclaw-memory list_unified_topics --
```
Expected: 3 PASS

跑全部 memory 测试确认无回归：
```
cargo test -p zeroclaw-memory
```
Expected: 全部 PASS

- [ ] **Step 5: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-memory
cargo clippy -p zeroclaw-memory --all-targets -- -D warnings
git add crates/zeroclaw-memory/src/sqlite.rs
git commit -m "feat(memory): add SqliteMemory::list_unified_topics

Returns distinct topic suffixes from session_ids matching
'unified_<master_id>_<topic>'. Used by /topic list to enumerate a
superuser's DawnIM topics."
```

---

## Task 5: `resolve_effective_topic` 纯函数 + 单测

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1: 在 `resolve_session_key` 旁加入测试 (在 mod.rs 现有的 tests module 内)**

先找现有 tests module 起始位置：
```
grep -n "^mod tests\|^#\[cfg(test)\]" crates/zeroclaw-channels/src/orchestrator/mod.rs | head -5
```

在其中追加：

```rust
    #[test]
    fn resolve_effective_topic_master_uses_thread_ts_only() {
        let msg = test_msg("dawnim", Some("work"), Some("topic_A"), "u_alice");
        let result = resolve_effective_topic(&msg, "dawnim.work", Some("dawnim.work"), None);
        assert_eq!(result, Some("topic_A".to_string()));
    }

    #[test]
    fn resolve_effective_topic_master_ignores_binding_even_when_set() {
        let msg = test_msg("dawnim", Some("work"), Some("topic_A"), "u_alice");
        let reg = test_registry();
        reg.set("dawnim.work", "u_alice", "binding_topic");
        let result = resolve_effective_topic(&msg, "dawnim.work", Some("dawnim.work"), Some(&reg));
        assert_eq!(result, Some("topic_A".to_string()), "master must ignore binding");
    }

    #[test]
    fn resolve_effective_topic_slave_prefers_thread_ts_over_binding() {
        let msg = test_msg("feishu", Some("work"), Some("native_topic"), "u_alice");
        let reg = test_registry();
        reg.set("feishu.work", "u_alice", "bound_topic");
        let result = resolve_effective_topic(&msg, "feishu.work", Some("dawnim.work"), Some(&reg));
        assert_eq!(result, Some("native_topic".to_string()));
    }

    #[test]
    fn resolve_effective_topic_slave_uses_binding_when_no_thread_ts() {
        let msg = test_msg("feishu", Some("work"), None, "u_alice");
        let reg = test_registry();
        reg.set("feishu.work", "u_alice", "bound_topic");
        let result = resolve_effective_topic(&msg, "feishu.work", Some("dawnim.work"), Some(&reg));
        assert_eq!(result, Some("bound_topic".to_string()));
    }

    #[test]
    fn resolve_effective_topic_returns_none_when_neither_set() {
        let msg = test_msg("feishu", Some("work"), None, "u_alice");
        let result = resolve_effective_topic(&msg, "feishu.work", Some("dawnim.work"), None);
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_effective_topic_no_master_config_falls_to_thread_ts() {
        let msg = test_msg("feishu", Some("work"), Some("native_topic"), "u_alice");
        let result = resolve_effective_topic(&msg, "feishu.work", None, None);
        assert_eq!(result, Some("native_topic".to_string()));
    }
```

辅助测试函数（如果还没有，在 tests module 顶部加）：

```rust
    fn test_msg(
        channel: &str,
        alias: Option<&str>,
        thread_ts: Option<&str>,
        sender: &str,
    ) -> zeroclaw_api::channel::ChannelMessage {
        let mut m = zeroclaw_api::channel::ChannelMessage::default();
        m.channel = channel.to_string();
        m.channel_alias = alias.map(|s| s.to_string());
        m.thread_ts = thread_ts.map(|s| s.to_string());
        m.sender = sender.to_string();
        m.reply_target = "r1".to_string();
        m.id = "m1".to_string();
        m
    }

    fn test_registry() -> zeroclaw_infra::topic_binding::TopicBindingRegistry {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tb.json");
        let _ = Box::leak(Box::new(tmp));
        zeroclaw_infra::topic_binding::TopicBindingRegistry::load(path).unwrap()
    }
```

若 `ChannelMessage::default()` 不存在或缺字段，按其实际签名手动填全字段。

- [ ] **Step 2: 实现函数**

在 `resolve_session_key` 紧邻上方（线 ~580 区域）插入：

```rust
/// Resolve the *effective* topic for an inbound channel message:
/// `msg.thread_ts` always wins on the master channel; on slave channels,
/// `thread_ts` (if set) wins, else the `topic_binding` registry fills in.
/// Returns `None` when neither source has a topic.
///
/// Centralises the "what topic should this message belong to" decision so
/// `resolve_session_key` and `ChannelOrigin.topic` stay consistent.
fn resolve_effective_topic(
    msg: &zeroclaw_api::channel::ChannelMessage,
    channel_ref: &str,
    master_channel_ref: Option<&str>,
    topic_binding: Option<&zeroclaw_infra::topic_binding::TopicBindingRegistry>,
) -> Option<String> {
    if master_channel_ref == Some(channel_ref) {
        return msg.thread_ts.clone();
    }
    msg.thread_ts.clone().or_else(|| {
        topic_binding.and_then(|reg| reg.get(channel_ref, &msg.sender))
    })
}
```

- [ ] **Step 3: 运行测试**

```
cargo test -p zeroclaw-channels resolve_effective_topic --
```
Expected: 6 PASS

- [ ] **Step 4: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-channels
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```

```
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): add resolve_effective_topic helper

Centralises 'which topic does this inbound message belong to' rule:
master uses thread_ts only; slave prefers thread_ts then falls back
to TopicBindingRegistry. Pure function with full matrix coverage."
```

---

## Task 6: `ChannelRuntimeCommand::Topic` + `TopicAction` + parse 扩展

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1: 写解析测试 (失败)**

定位现有 `parse_runtime_command_recognizes_bind_and_unbind` 测试附近，新增：

```rust
    #[test]
    fn parse_runtime_command_recognizes_topic_help() {
        assert!(matches!(
            parse_runtime_command("feishu", "/topic"),
            Some(ChannelRuntimeCommand::Topic(TopicAction::Help))
        ));
    }

    #[test]
    fn parse_runtime_command_recognizes_topic_list() {
        assert!(matches!(
            parse_runtime_command("feishu", "/topic list"),
            Some(ChannelRuntimeCommand::Topic(TopicAction::List))
        ));
    }

    #[test]
    fn parse_runtime_command_recognizes_topic_reset() {
        assert!(matches!(
            parse_runtime_command("feishu", "/topic reset"),
            Some(ChannelRuntimeCommand::Topic(TopicAction::Reset))
        ));
    }

    #[test]
    fn parse_runtime_command_recognizes_topic_set() {
        match parse_runtime_command("feishu", "/topic db_lock") {
            Some(ChannelRuntimeCommand::Topic(TopicAction::Set(id))) => {
                assert_eq!(id, "db_lock");
            }
            other => panic!("expected Topic(Set), got {:?}", other),
        }
    }

    #[test]
    fn parse_runtime_command_topic_set_rejects_extra_tokens() {
        // Multiple non-keyword tokens after /topic -> treated as Help (invalid args)
        assert!(matches!(
            parse_runtime_command("feishu", "/topic db_lock extra"),
            Some(ChannelRuntimeCommand::Topic(TopicAction::Help))
        ));
    }

    #[test]
    fn parse_runtime_command_topic_keywords_case_insensitive() {
        assert!(matches!(
            parse_runtime_command("feishu", "/topic LIST"),
            Some(ChannelRuntimeCommand::Topic(TopicAction::List))
        ));
        assert!(matches!(
            parse_runtime_command("feishu", "/topic Reset"),
            Some(ChannelRuntimeCommand::Topic(TopicAction::Reset))
        ));
    }
```

- [ ] **Step 2: 扩展 enum + parse_runtime_command**

修改 `ChannelRuntimeCommand` enum（mod.rs:297 附近）：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum ChannelRuntimeCommand {
    ShowProviders,
    SetProvider(String),
    ShowModel,
    SetModel(String),
    ShowConfig,
    NewSession,
    Bind(Option<String>),
    Unbind,
    Topic(TopicAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TopicAction {
    Help,            // `/topic`
    List,            // `/topic list`
    Reset,           // `/topic reset`
    Set(String),     // `/topic <id>`
}
```

修改 `parse_runtime_command`（mod.rs:1235）的 match 表中加入 `/topic` 分支，紧邻 `/unbind` 后：

```rust
        "/topic" => {
            // Collect remaining tokens; 0 -> Help, 1 -> List/Reset/Set, 2+ -> Help (invalid).
            let rest: Vec<&str> = parts.collect();
            match rest.len() {
                0 => Some(ChannelRuntimeCommand::Topic(TopicAction::Help)),
                1 => {
                    let token = rest[0].trim();
                    let action = match token.to_ascii_lowercase().as_str() {
                        "list" => TopicAction::List,
                        "reset" => TopicAction::Reset,
                        "help" => TopicAction::Help,
                        _ => TopicAction::Set(token.to_string()),
                    };
                    Some(ChannelRuntimeCommand::Topic(action))
                }
                _ => Some(ChannelRuntimeCommand::Topic(TopicAction::Help)),
            }
        }
```

`handle_runtime_command_if_needed` 的 match 表会因新增 variant 编译失败 — 先临时加占位：

```rust
        ChannelRuntimeCommand::Topic(_) => {
            // Implemented in Task 8.
            "TODO: /topic handler".to_string()
        }
```

(占位会在 Task 8 被替换。这里写注释明确暂存。)

- [ ] **Step 3: 运行测试**

```
cargo test -p zeroclaw-channels parse_runtime_command_recognizes_topic --
cargo test -p zeroclaw-channels parse_runtime_command_topic --
```
Expected: 6 PASS

确保旧测试也过：
```
cargo test -p zeroclaw-channels parse_runtime_command --
```
Expected: 全部 PASS

- [ ] **Step 4: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-channels
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```

```
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): parse /topic slash command into TopicAction

Adds ChannelRuntimeCommand::Topic(TopicAction) variant with Help / List /
Reset / Set(id) sub-actions. Handler body is a placeholder; the real
implementation lands in the following commits."
```

---

## Task 7: `ChannelRuntimeContext.topic_binding` + 启动期装配

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1: 添加字段**

修改 `ChannelRuntimeContext` (mod.rs:431) 紧邻 `identity` 字段后加：

```rust
    identity: Option<Arc<IdentityRuntime>>,
    /// Process-wide topic binding registry. Present iff identity is
    /// present (same gate: `[channels].master_channel` configured).
    /// Read by `process_channel_message_body` to compute effective_topic;
    /// mutated by `/topic` command handler.
    topic_binding: Option<Arc<zeroclaw_infra::topic_binding::TopicBindingRegistry>>,
```

(找到 `identity:` 字段位置；它在结构体后段。grep 行号：`grep -n "identity: Option<Arc<IdentityRuntime" crates/zeroclaw-channels/src/orchestrator/mod.rs`)

- [ ] **Step 2: 启动期构造**

`shared_identity` 之后紧接（mod.rs:8580+）：

```rust
    // Process-wide topic binding store, gated on master_channel like
    // shared_identity. Disabled (None) otherwise.
    let shared_topic_binding: Option<Arc<zeroclaw_infra::topic_binding::TopicBindingRegistry>> =
        if shared_identity.is_some() {
            match zeroclaw_infra::make_topic_binding_registry(&config.data_dir) {
                Ok(reg) => Some(reg),
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "topic_binding registry init failed; /topic disabled"
                    );
                    None
                }
            }
        } else {
            None
        };
```

- [ ] **Step 3: 注入到所有 ChannelRuntimeContext 构造点**

找出每处构造 `ChannelRuntimeContext { ... identity: shared_identity.clone(), ... }`：
```
grep -n "identity: shared_identity" crates/zeroclaw-channels/src/orchestrator/mod.rs
```

在每处 `identity: shared_identity.clone(),` 旁加：

```rust
            identity: shared_identity.clone(),
            topic_binding: shared_topic_binding.clone(),
```

可能有 1-2 处（agent ctx + 测试 ctx）。

- [ ] **Step 4: 修复测试中构造 ChannelRuntimeContext 的位置**

如果 mod.rs 内部测试或 helper 构造 `ChannelRuntimeContext`，按字段新增添加 `topic_binding: None,`。grep：
```
grep -n "ChannelRuntimeContext {" crates/zeroclaw-channels/src/orchestrator/mod.rs
```

- [ ] **Step 5: 编译并跑测试**

```
cargo build -p zeroclaw-channels
cargo test -p zeroclaw-channels
```
Expected: 编译过；测试全部 PASS（应该没有新行为引入）

- [ ] **Step 6: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-channels
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```

```
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): wire shared_topic_binding into ChannelRuntimeContext

Builds the registry alongside shared_identity (same master_channel gate)
and threads Arc<TopicBindingRegistry> through every agent ctx. Handler
body still TODO."
```

---

## Task 8: `handle_runtime_command_if_needed::Topic` 完整实现

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1: 写 handler 单元测试 (失败)**

由于 handler 涉及外部 IO（SqliteMemory），先用结构化的小测试覆盖各响应路径。在 tests module 中：

```rust
    #[test]
    fn topic_help_response_lists_subcommands() {
        let body = build_topic_help_response();
        assert!(body.contains("/topic list"));
        assert!(body.contains("/topic <"));
        assert!(body.contains("/topic reset"));
        assert!(body.contains("superuser"));
    }

    #[test]
    fn topic_list_response_with_entries_marks_current_binding() {
        let body = build_topic_list_response(
            "dawnim.work",
            &["db_lock".to_string(), "migrations".to_string()],
            Some("db_lock"),
        );
        assert!(body.contains("dawnim.work"));
        assert!(body.contains("db_lock"));
        assert!(body.contains("当前绑定"));
        assert!(body.contains("migrations"));
    }

    #[test]
    fn topic_list_response_empty_explains_how_to_create() {
        let body = build_topic_list_response("dawnim.work", &[], None);
        assert!(body.contains("dawnim.work"));
        assert!(body.contains("尚无") || body.contains("没有"));
    }
```

- [ ] **Step 2: 实现 helper 字符串构造函数 (顶层)**

紧邻 `handle_runtime_command_if_needed` 上方加：

```rust
fn build_topic_help_response() -> String {
    "用法：\n  /topic list        查看 master 渠道上的所有话题\n  /topic <名称>      把当前渠道绑定到指定话题\n  /topic reset       解除绑定，恢复独立会话\n\n仅 superuser 可用。"
        .to_string()
}

fn build_topic_list_response(
    master_channel: &str,
    topics: &[String],
    current: Option<&str>,
) -> String {
    if topics.is_empty() {
        return format!(
            "你在 {master_channel} 尚无任何话题。请先在 dawnim 客户端创建话题并发送消息。"
        );
    }
    let mut s = format!(
        "你在 {master_channel} 的话题（共 {} 个）：\n",
        topics.len()
    );
    for t in topics {
        if Some(t.as_str()) == current {
            s.push_str(&format!("  • {t}         ← 当前绑定\n"));
        } else {
            s.push_str(&format!("  • {t}\n"));
        }
    }
    s.push_str("\n用法：/topic <名称> 绑定，/topic reset 解绑");
    s
}
```

- [ ] **Step 3: 实现 `list_master_topics` helper**

紧邻 `resolve_effective_topic` 后插入：

```rust
/// Query the master DawnIM channel's SqliteMemory for topics belonging
/// to `master_id`. Parses `[channels].master_channel` (form
/// `<type>.<alias>`) to derive the per-alias memory db name
/// (`dawn_im_<alias>`). Returns empty + warning on any failure so
/// the caller can still reply gracefully.
fn list_master_topics(
    ctx: &ChannelRuntimeContext,
    master_id: &str,
) -> anyhow::Result<Vec<String>> {
    let Some(identity) = ctx.identity.as_deref() else {
        return Ok(Vec::new());
    };
    // master_channel example: "dawnim.work" -> alias "work"
    let alias = identity
        .master_channel
        .rsplit('.')
        .next()
        .ok_or_else(|| anyhow::anyhow!("master_channel has no alias suffix"))?;
    let db_name = format!("dawn_im_{alias}");
    let mem = zeroclaw_memory::SqliteMemory::new_named(
        "topic_list",
        ctx.workspace_dir.as_ref(),
        &db_name,
    )?;
    mem.list_unified_topics(master_id)
}
```

`zeroclaw_memory::SqliteMemory` 在 orchestrator 中是否已导入？grep：
```
grep -n "zeroclaw_memory::SqliteMemory\|use zeroclaw_memory" crates/zeroclaw-channels/src/orchestrator/mod.rs | head -5
```
若无，加 `use zeroclaw_memory::SqliteMemory;` 至 mod.rs 顶部 imports。

- [ ] **Step 4: 替换 Task 6 占位实现 + 添加 async helper**

定位 Task 6 留下的 `ChannelRuntimeCommand::Topic(_) => { "TODO ...".to_string() }`，替换为：

```rust
        ChannelRuntimeCommand::Topic(action) => topic_handler_response(ctx, msg, action).await,
```

紧邻 `handle_runtime_command_if_needed` 上方（与 build_topic_* helpers 同处）新增 async 函数：

```rust
async fn topic_handler_response(
    ctx: &ChannelRuntimeContext,
    msg: &zeroclaw_api::channel::ChannelMessage,
    action: TopicAction,
) -> String {
    let identity = match ctx.identity.as_deref() {
        Some(i) => i,
        None => return "/topic 仅 superuser 可用。".to_string(),
    };
    let channel_ref = match &msg.channel_alias {
        Some(alias) => format!("{}.{}", msg.channel, alias),
        None => msg.channel.clone(),
    };
    let is_master = channel_ref == identity.master_channel;
    let master_id = match identity
        .resolver
        .resolve(&channel_ref, &msg.sender, is_master)
    {
        Some(id) => id,
        None => return "/topic 仅 superuser 可用。".to_string(),
    };
    let binding_reg = match ctx.topic_binding.as_deref() {
        Some(r) => r,
        None => return "/topic 未启用。".to_string(),
    };

    match action {
        TopicAction::Help => build_topic_help_response(),
        TopicAction::Reset => {
            if binding_reg.clear(&channel_ref, &msg.sender) {
                "已清除话题绑定。本渠道恢复独立会话。".to_string()
            } else {
                "当前没有话题绑定。".to_string()
            }
        }
        TopicAction::List => {
            let topics = match list_master_topics(ctx, &master_id) {
                Ok(v) => v,
                Err(e) => {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                        "topic list query failed; returning empty"
                    );
                    Vec::new()
                }
            };
            let current = binding_reg.get(&channel_ref, &msg.sender);
            build_topic_list_response(&identity.master_channel, &topics, current.as_deref())
        }
        TopicAction::Set(id) => {
            let id = id.trim().to_string();
            if id.is_empty() {
                return build_topic_help_response();
            }
            let topics = list_master_topics(ctx, &master_id).unwrap_or_default();
            if !topics.iter().any(|t| t == &id) {
                format!("话题 \"{id}\" 不存在。运行 /topic list 查看可用话题。")
            } else if binding_reg.get(&channel_ref, &msg.sender).as_deref() == Some(id.as_str()) {
                format!("已绑定到话题 \"{id}\"（无变化）。")
            } else {
                binding_reg.set(&channel_ref, &msg.sender, &id);
                format!("已绑定到话题 \"{id}\"。本渠道后续消息将归入该话题的对话历史。")
            }
        }
    }
}
```

**关键 Rust 细节**：Rust 的 `let-else` 要求 else 分支 diverge (return/break/panic)。本实现用 `match { Some => ..., None => return ... }` 而非 `let-else`，避免编译错误。

- [ ] **Step 5: 运行测试**

```
cargo test -p zeroclaw-channels topic_ --
cargo test -p zeroclaw-channels
```
Expected: 新增 3 个 helper 测试 PASS；全部 mod.rs 测试 PASS

- [ ] **Step 6: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-channels
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```

```
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): implement /topic command handler

Handles List / Set / Reset / Help via topic_handler_response.
Permission gate: identity must resolve sender to a master_id (i.e.
superuser, either on master or post-/bind on a slave). Topic lookup
queries the master DawnIM's SqliteMemory and validates the id before
binding. All replies in Chinese, matching /bind / /unbind style."
```

---

## Task 9: `process_channel_message_body` 集成 — effective_topic 下发到 session_key 与 ChannelOrigin

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1: 扩展 `resolve_session_key` 签名以接受 `effective_topic`，让 master 的 phase0 修复继续工作**

现有签名 (mod.rs:587)：

```rust
fn resolve_session_key(
    msg: &zeroclaw_api::channel::ChannelMessage,
    identity: Option<&IdentityRuntime>,
) -> String
```

替换为：

```rust
fn resolve_session_key(
    msg: &zeroclaw_api::channel::ChannelMessage,
    identity: Option<&IdentityRuntime>,
    effective_topic: Option<&str>,
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
    match identity
        .resolver
        .resolve(&channel_ref, &msg.sender, is_master)
    {
        Some(master_id) => match effective_topic {
            Some(topic) if !topic.is_empty() => {
                sanitize_session_key(&format!("unified_{master_id}_{topic}"))
            }
            _ => sanitize_session_key(&format!("unified_{master_id}")),
        },
        None => base,
    }
}
```

（**注意**：这里把"读 `msg.thread_ts`"改成"读 `effective_topic`"——所有调用点必须传 effective_topic。)

- [ ] **Step 2: 更新所有调用点**

```
grep -n "resolve_session_key(" crates/zeroclaw-channels/src/orchestrator/mod.rs
```

每一处调用都要改为传 effective_topic。两类调用点：

**(a) `process_channel_message_body` (mod.rs:4037)** — 这是核心改动：

```rust
    let effective_topic = resolve_effective_topic(
        &msg,
        &channel_ref_string_for_msg(&msg),  // helper below
        ctx.identity.as_ref().map(|i| i.master_channel.as_str()),
        ctx.topic_binding.as_deref(),
    );
    let history_key = resolve_session_key(
        &msg,
        ctx.identity.as_deref(),
        effective_topic.as_deref(),
    );
```

需要新增辅助 `channel_ref_string_for_msg` — 或者直接 inline 计算：

```rust
    let channel_ref_for_msg = match &msg.channel_alias {
        Some(alias) => format!("{}.{}", msg.channel, alias),
        None => msg.channel.clone(),
    };
    let effective_topic = resolve_effective_topic(
        &msg,
        &channel_ref_for_msg,
        ctx.identity.as_ref().map(|i| i.master_channel.as_str()),
        ctx.topic_binding.as_deref(),
    );
    let history_key = resolve_session_key(
        &msg,
        ctx.identity.as_deref(),
        effective_topic.as_deref(),
    );
```

**(b) `handle_runtime_command_if_needed` (mod.rs:2459)**：

```rust
    let sender_key = resolve_session_key(msg, ctx.identity.as_deref(), msg.thread_ts.as_deref());
```

(此处用 msg.thread_ts 即可，因为 command handler 不需要 binding override — command 处理本身就在 master 拦截之外，对 slave 影响小；保持简单。)

**(c) 测试内 `resolve_session_key` 调用** (mod.rs:16674, 16799 等)：每处加 `None` 第三参数（向后兼容）：

```rust
    let history_key = resolve_session_key(&msg, None, None);
```

或者改用 `conversation_history_key(&msg)` 直接 — 但保留原意更稳。检查 each call site。

- [ ] **Step 3: 更新 ChannelOrigin.topic 填充**

mod.rs:4691 处 `let channel_origin = zeroclaw_api::channel::ChannelOrigin { ... topic: msg.thread_ts.clone() }`，改为：

```rust
    let channel_origin = zeroclaw_api::channel::ChannelOrigin {
        from_uid: msg
            .sender
            .split("_la_")
            .next()
            .unwrap_or(msg.sender.as_str())
            .to_string(),
        reply_target: msg.reply_target.clone(),
        channel_ref: msg
            .channel_alias
            .as_ref()
            .map(|a| format!("{}.{}", msg.channel, a))
            .unwrap_or_else(|| msg.channel.clone()),
        topic: effective_topic.clone(),
    };
```

- [ ] **Step 4: 添加 session_key 集成测试**

在 tests module 中：

```rust
    #[test]
    fn resolve_session_key_uses_effective_topic_over_thread_ts() {
        // Construct an IdentityRuntime with a stub resolver that returns Some.
        struct AlwaysResolves;
        impl zeroclaw_infra::identity_store::IdentityResolver for AlwaysResolves {
            fn resolve(&self, _ch: &str, sender: &str, _m: bool) -> Option<String> {
                Some(sender.to_string())
            }
            fn issue_code(&self, _: &str) -> Option<String> { None }
            fn redeem_code(&self, _: &str, _: &str, _: &str) -> Result<String, String> {
                Err("n/a".into())
            }
            fn unbind(&self, _: &str, _: &str) -> bool { false }
        }
        let identity = IdentityRuntime {
            resolver: Arc::new(AlwaysResolves),
            master_channel: "dawnim.work".to_string(),
        };
        let msg = test_msg("feishu", Some("work"), Some("ignored_native"), "u_alice");
        let key = resolve_session_key(&msg, Some(&identity), Some("bound_topic"));
        assert_eq!(key, sanitize_session_key("unified_u_alice_bound_topic"));
    }

    #[test]
    fn resolve_session_key_unified_no_topic_when_effective_topic_none() {
        struct AlwaysResolves;
        impl zeroclaw_infra::identity_store::IdentityResolver for AlwaysResolves {
            fn resolve(&self, _ch: &str, sender: &str, _m: bool) -> Option<String> {
                Some(sender.to_string())
            }
            fn issue_code(&self, _: &str) -> Option<String> { None }
            fn redeem_code(&self, _: &str, _: &str, _: &str) -> Result<String, String> {
                Err("n/a".into())
            }
            fn unbind(&self, _: &str, _: &str) -> bool { false }
        }
        let identity = IdentityRuntime {
            resolver: Arc::new(AlwaysResolves),
            master_channel: "dawnim.work".to_string(),
        };
        let msg = test_msg("feishu", Some("work"), None, "u_alice");
        let key = resolve_session_key(&msg, Some(&identity), None);
        assert_eq!(key, sanitize_session_key("unified_u_alice"));
    }
```

`use zeroclaw_api::session_keys::sanitize_session_key;` 若未在 tests 顶部，添加。

- [ ] **Step 5: 编译并跑测试**

```
cargo build -p zeroclaw-channels
cargo test -p zeroclaw-channels resolve_session_key --
cargo test -p zeroclaw-channels
```
Expected: 全部 PASS（旧的 resolve_session_key 测试需要更新签名 — 修复后通过）

- [ ] **Step 6: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-channels
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```

```
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): plumb effective_topic into session_key + ChannelOrigin

process_channel_message_body now computes effective_topic via the new
resolver and feeds it to both resolve_session_key (third arg) and
ChannelOrigin.topic. Master channels keep using thread_ts (binding
ignored); slaves get binding-fallback. Phase-0 unified_<master>_<topic>
key shape preserved when topic is present, falls to unified_<master>
when absent."
```

---

## Task 10: End-to-end smoke test (integration)

**Files:**
- Create: `crates/zeroclaw-channels/tests/topic_command_e2e.rs`

- [ ] **Step 1: 写最小可行的端到端测试**

检查 zeroclaw-channels 是否已有 `tests/` 目录：
```
ls crates/zeroclaw-channels/tests 2>/dev/null
```

如果是首次，先确认本测试需要的所有 helpers 都可从 lib 访问；本任务的目标是写一个**纯函数链**测试，不依赖真实 orchestrator 启动。

```rust
//! End-to-end check that /topic binding flows through resolve_effective_topic
//! and resolve_session_key to produce the expected unified session key.

use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn feishu_bound_to_topic_routes_to_unified_master_topic_key() {
    let tmp = TempDir::new().unwrap();
    let reg = zeroclaw_infra::make_topic_binding_registry(tmp.path()).unwrap();
    reg.set("feishu.work", "ou_alice", "db_lock");

    // Reload to simulate restart — binding must persist.
    drop(reg);
    let reg = zeroclaw_infra::make_topic_binding_registry(tmp.path()).unwrap();
    assert_eq!(
        reg.get("feishu.work", "ou_alice"),
        Some("db_lock".to_string()),
        "binding must survive registry reload"
    );
}

#[test]
fn topic_binding_is_per_channel_sender_pair() {
    let tmp = TempDir::new().unwrap();
    let reg = zeroclaw_infra::make_topic_binding_registry(tmp.path()).unwrap();
    reg.set("feishu.work", "ou_alice", "topic_a");
    reg.set("feishu.work", "ou_bob", "topic_b");
    reg.set("wecom_ws.default", "ou_alice", "topic_c");

    assert_eq!(reg.get("feishu.work", "ou_alice"), Some("topic_a".into()));
    assert_eq!(reg.get("feishu.work", "ou_bob"), Some("topic_b".into()));
    assert_eq!(reg.get("wecom_ws.default", "ou_alice"), Some("topic_c".into()));
}

#[test]
fn topic_listing_returns_only_master_ids_topics() {
    use zeroclaw_memory::SqliteMemory;
    use zeroclaw_memory::traits::MemoryCategory;
    let tmp = TempDir::new().unwrap();
    let mem = SqliteMemory::new_named("test", tmp.path(), "dawn_im_work").unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        for sid in [
            "unified_u_alice_db_lock",
            "unified_u_alice_migrations",
            "unified_u_bob_secret",
        ] {
            use zeroclaw_memory::traits::Memory;
            mem.store("k", "x", MemoryCategory::default(), Some(sid.to_string()))
                .await
                .unwrap();
        }
    });
    let mut topics = mem.list_unified_topics("u_alice").unwrap();
    topics.sort();
    assert_eq!(topics, vec!["db_lock", "migrations"]);
    let bob = mem.list_unified_topics("u_bob").unwrap();
    assert_eq!(bob, vec!["secret"]);
}
```

确认 `[dev-dependencies]` 含 `tempfile`、`tokio`（带 `rt` feature）。如果 zeroclaw-channels Cargo.toml 没有，添加：

```toml
[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["rt", "macros"] }
zeroclaw-infra = { path = "../zeroclaw-infra" }
zeroclaw-memory = { path = "../zeroclaw-memory" }
```

检查现有内容：
```
grep -n "tempfile\|tokio\|zeroclaw-infra\|zeroclaw-memory" crates/zeroclaw-channels/Cargo.toml
```

- [ ] **Step 2: 运行测试**

```
cargo test -p zeroclaw-channels --test topic_command_e2e
```
Expected: 3 PASS

- [ ] **Step 3: 跑整个 zeroclaw-channels 测试套件确认无回归**

```
cargo test -p zeroclaw-channels
```
Expected: 全部 PASS（修复掉的 8 个 pre-existing 失败如果还在，告知用户）

- [ ] **Step 4: fmt + clippy + commit**

```
cargo fmt -p zeroclaw-channels
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```

```
git add crates/zeroclaw-channels/tests/topic_command_e2e.rs crates/zeroclaw-channels/Cargo.toml
git commit -m "test(channels): e2e for /topic binding persistence and listing

Covers: binding survives registry reload; per-(channel, sender)
isolation; list_unified_topics scopes by master_id."
```

---

## Task 11: 全仓最终验证 + 文档同步

**Files:**
- Modify: `docs/maintainers/migration-tracking-TBD.md` (可选)

- [ ] **Step 1: 全仓构建 + 测试**

```
cargo build --workspace
cargo test -p zeroclaw-infra -p zeroclaw-memory -p zeroclaw-channels
```
Expected: 编译过；测试通过（或仅留下 baseline pre-existing 失败）

- [ ] **Step 2: 全仓 clippy**

```
cargo clippy --workspace --all-targets -- -D warnings
```
Expected: no warnings（如有 pre-existing，对比 baseline 不增）

- [ ] **Step 3: (可选) 在 migration-tracking-TBD.md 末尾追加 /topic 段，记录跨渠道工具栈对话能力的完成**

仅当用户认可此条改动后再 commit。否则跳过。

- [ ] **Step 4: 最终 commit（如有文档改动）**

```
git add docs/maintainers/migration-tracking-TBD.md
git commit -m "docs(maintainers): note /topic cross-channel binding ships"
```

---

## Validation Checklist

实施全部完成后核对：

- [ ] feishu user `/bind` + `/topic db_lock` → 后续 feishu 消息 session_key 包含 `unified_<master>_db_lock`
- [ ] `/topic reset` → 后续消息 session_key 回到 `unified_<master>`（无 topic 后缀）
- [ ] `/topic list` 在 master_id 下正确列出 topic，标注当前绑定
- [ ] `/topic <未知 id>` 拒绝并提示运行 /topic list
- [ ] `/topic` (无 arg) 显示 Help
- [ ] 非 superuser 的 `/topic *` → "/topic 仅 superuser 可用"
- [ ] master 渠道（DawnIM）即使 `topic_binding` 有记录也不生效（thread_ts 优先）
- [ ] 重启后 binding 从 JSON 恢复
- [ ] ChannelOrigin.topic 在工具栈中拿到的是 effective_topic（不是原始 msg.thread_ts）
- [ ] 无 AI 属性 trailer (`Co-Authored-By: Claude`) 出现在任何 commit 中

---

## Risk / Rollback Notes

- **如果 `resolve_session_key` 签名变更影响其他 crate 调用方**：grep 全仓 `resolve_session_key`；本函数定义在 orchestrator/mod.rs 内是 `fn` (not `pub`)，理应没有外部调用；但如有，每处加第三参 `None`
- **如果 `dawn_im_<alias>` SqliteMemory 在 list 调用时与 channel 已开的实例并发**：SqliteMemory 内部用 connection mutex；多实例打开同一 db 是合法的（SQLite WAL 模式），但若出现锁竞争超时，将 list_master_topics 改为复用 ctx.memory（仅当 ctx.memory 恰好就是 dawn_im_<alias> 实例时）。本期保持简单：每次 list 单独 open——/topic list 不是高频路径
- **回退**：删除 `topic_binding.json` + revert 本系列 commits 即可恢复原行为；代码层 `topic_binding: None` 的 ctx 走原 thread_ts-only 路径
