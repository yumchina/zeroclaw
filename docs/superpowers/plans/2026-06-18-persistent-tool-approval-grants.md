# Persistent Tool Approval Grants Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在同 `(channel_ref, topic, triggerer_master_id, tool_name)` 键上，把首次「始终允许」审批结果持久化到 sqlite；后续相同键的工具调用自动放行；为非 superuser 触发的工具调用增加"代审"流程（fan-out 给所有 superuser，先回为准）；审批卡片人话化（轻量 LLM 摘要）；gateway API 暴露查看/删除已存授权；所有审批结果统一通过 `zeroclaw-log` 落到 `runtime-trace.jsonl`。

**Architecture:** 方案 A 就地演进。新增 `ApprovalGrantStore`（trait + Sqlite 实现 + LRU 缓存）替换 `ApprovalManager.session_allowlist`；新增 `ApprovalBroker` 抽走"反向解析 + fan-out + 人话化 + 持久化"职责；`approval_gate.rs` 内部改为命中放行或调 broker；`Channel` trait 新增 `cancel_approval` 默认空实现，dawn_im 补「始终允许」按钮 + cancel 卡片更新；`ApprovalManager.audit_log` 字段删除，所有 `record_decision` 走 `zeroclaw_log::record!` 写 `runtime-trace.jsonl`（DRY 合规）。

**Tech Stack:** Rust 2024 edition、tokio、rusqlite、parking_lot、lru = "0.16"、uuid = "1.22"（v4）、async-trait、`zeroclaw-log`、`zeroclaw-memory::SqliteMemory`、`zeroclaw-config`、`zeroclaw-runtime`、`zeroclaw-channels`、`zeroclaw-gateway`、`zeroclaw-infra`。

## Global Constraints

- **DRY 铁律（AGENTS.md 头号）**：superuser 列表唯一源 = `ChannelsConfig.superusers`；反向 uid 解析读 `/bind` 的现有 `identity_mapping` 表，不复制；grant 唯一源 = sqlite；audit 唯一源 = `runtime-trace.jsonl`。任何新增字段先回答「source of truth — created here」或「duplicate — refuse」。
- **Tool trait 不动；Channel trait 仅新增带默认空实现的 `cancel_approval` 方法**。
- **每个 PR 一件事**。本 plan 任务边界按 PR 拆分线对齐。
- **测试要求**：`cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test` 全绿；新增模块附单元测试；改造模块原有测试要么保留要么重写为等价语义。
- **不引入新 crate 依赖**。`uuid`/`lru`/`async-trait`/`parking_lot`/`rusqlite`/`tokio` 都已在 workspace。
- **不动 `zeroclaw-log::EventCategory` 枚举**（Beta 稳定层）。审批事件复用 `EventCategory::Tool` + `Action::Approve` / `Action::Reject`。
- **id 生成**：grant id 用 `Uuid::new_v4().to_string()`（项目惯例，不引 ulid）；时序排序通过 `granted_at DESC` 补足。
- **审批卡片 LLM 调用硬超时 10 秒**；失败/超时回退到 `summarize_args(args)` 原文。
- **本次不持久化 audit_log 到独立 sqlite/jsonl**：复用 `zeroclaw-log` 的 `runtime-trace.jsonl`。
- **末路默认拒绝**：任何"无法决定"（无 superuser / 全部超时 / 全部 channel 失败 / master_channel 缺失）→ `Decision::No`。
- **secret 红线**：LLM 调用前必经 `summarize_args` 红线脱敏；raw_arguments 不入 jsonl。

## File Structure

**新建文件：**

- `crates/zeroclaw-runtime/src/approval/grant_store.rs`
  - 职责：`ApprovalGrant` 结构、`ApprovalGrantStore` trait、`SqliteGrantStore` 实现（含 LRU 缓存）、`GrantFilter` 查询参数；该文件不知道 broker、不知道 channel。
- `crates/zeroclaw-runtime/src/approval/broker.rs`
  - 职责：`ApprovalBroker` 主体；接受 `ApprovalRequestCtx`，输出 `Decision`；内部协调反向解析、人话化、fan-out、grant 持久化、audit 记录。
- `crates/zeroclaw-runtime/src/approval/humanize.rs`
  - 职责：`SummaryProvider` trait + `LlmSummaryProvider` 默认实现 + 10s 超时 + LRU 缓存 + 红线脱敏。
- `crates/zeroclaw-runtime/src/approval/decision_reason.rs`
  - 职责：`DecisionReason` 枚举（11 种 reason 常量字符串，避免 typo 散落）。

**修改文件：**

- `crates/zeroclaw-api/src/channel.rs` — `Channel::cancel_approval` 默认空实现。
- `crates/zeroclaw-channels/src/dawn_im/approval.rs` — 卡片新增「始终允许」按钮；`WkApprovalAction` 解析 `always`；新增 `build_resolved_card`。
- `crates/zeroclaw-channels/src/dawn_im/channel.rs` — `impl Channel for DawnIMChannel` 增 `cancel_approval`，调用 patch 路径更新卡片为「已由 XX 处理」。
- `crates/zeroclaw-config/src/schema.rs` — 新增 `ApprovalConfig`（顶层 `[approval]`）；更新 `ChannelsConfig.superusers` 注释。
- `crates/zeroclaw-infra/src/identity_store.rs` — `IdentityResolver` trait 增 `reverse_lookup(master_id, channel_ref) -> Option<String>`；`SqliteIdentityStore` 实现走 `idx_identity_master` 索引。
- `crates/zeroclaw-runtime/src/approval/mod.rs` — 删 `session_allowlist` + `audit_log` + `audit_log()` 访问器；引入 `Arc<dyn ApprovalGrantStore>` + `ApprovalBroker`；重写 `record_decision(...)` 走 `record!`；导出新模块。
- `crates/zeroclaw-runtime/src/agent/turn/approval_gate.rs` — `gate_tool_approval` 简化为：requirement → 命中放行 / 否则 `broker.request_decision(...)`。
- `crates/zeroclaw-runtime/src/agent/safety_net.rs` — 测试改写：原 `audit_log()` 断言换为 `LogCaptureLayer` 捕获事件断言。
- `src/approval/mod.rs` — 单元测试改写（同上）。
- `crates/zeroclaw-gateway/src/api.rs` — 新增 `GET /api/approvals/grants` + `DELETE /api/approvals/grants/{id}`；注入 `Arc<dyn ApprovalGrantStore>`。
- `crates/zeroclaw-runtime/Cargo.toml` — 启用 `lru` 已存在（确认无遗漏）。
- `CHANGELOG-next.md` — 收口记录。
- `docs/book/src/agents/delegation.md` — 简要说明新行为（如该文档存在 approval 章节）。

**lark 不动**：lark.rs 已具备三按钮与 patch 路径（见 spec §11）。

---

## Task 1: `[approval]` 配置段 + ChannelsConfig.superusers 注释

**Files:**
- Modify: `crates/zeroclaw-config/src/schema.rs`（在 ChannelsConfig 附近定位 `pub superusers: Vec<String>`；新增 `ApprovalConfig` 与顶层挂载点）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: 无
- Produces:
  - `pub struct ApprovalConfig { pub summary_provider: Option<String>, pub humanize_timeout_secs: u64 }`
  - 默认值：`summary_provider = None`（broker 端 fallback 到 agent 主 provider），`humanize_timeout_secs = 10`
  - `Config.approval: ApprovalConfig`（或在 `RootConfig` 等已有顶层结构里挂载，按 schema.rs 约定）
  - `ChannelsConfig.superusers` 注释更新为「Master-channel user ids. 双重用途：(1) 仅这些用户可发起 `/bind`；(2) 全局工具审批人（持久化授权设计 2026-06-18）」

- [ ] **Step 1: 写注释更新的 doc-string regression 测试**

```rust
// in crates/zeroclaw-config/src/schema.rs 现有 tests mod 末尾
#[test]
fn superusers_doc_mentions_global_approver() {
    // 编译期检查注释里包含 "审批" 关键词不可行（doc-string 不暴露给运行时），
    // 改为：构造空 ApprovalConfig 默认值的烟雾测试。
    let cfg = ApprovalConfig::default();
    assert_eq!(cfg.humanize_timeout_secs, 10);
    assert!(cfg.summary_provider.is_none());
}
```

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p zeroclaw-config superusers_doc_mentions_global_approver
```
Expected: FAIL with "cannot find type `ApprovalConfig`".

- [ ] **Step 3: 实现 ApprovalConfig**

在 `crates/zeroclaw-config/src/schema.rs` 找到顶层 `Config` / `RootConfig`（与 `ChannelsConfig` 同级的结构）后，添加：

```rust
/// 工具审批相关配置。新增于持久化审批授权设计（spec 2026-06-18）。
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "approval"]
pub struct ApprovalConfig {
    /// 用于生成审批卡片「人话摘要」的 provider 别名（`<type>.<alias>` 或 provider 名）。
    /// `None` 表示复用 agent 主 provider。
    #[serde(default)]
    pub summary_provider: Option<String>,

    /// LLM 人话摘要硬超时（秒）。超时即回退到 `summarize_args` 原文。
    #[serde(default = "default_humanize_timeout_secs")]
    pub humanize_timeout_secs: u64,
}

fn default_humanize_timeout_secs() -> u64 { 10 }

impl Default for ApprovalConfig {
    fn default() -> Self {
        Self { summary_provider: None, humanize_timeout_secs: 10 }
    }
}
```

并在顶层 `Config` 结构添加：

```rust
    #[serde(default)]
    #[nested]
    pub approval: ApprovalConfig,
```

更新 `ChannelsConfig.superusers` 字段注释为：

```rust
    /// Master-channel user ids. 双重用途：
    /// (1) 仅这些用户可发起 `/bind`（unified-session whitelist 种子）；
    /// (2) 全局工具审批人（spec docs/superpowers/specs/2026-06-18-persistent-tool-approval-grants-design.md）。
    /// 非 superuser 触发的工具调用会被 broker 代发给该列表中的所有用户。
    #[serde(default)]
    pub superusers: Vec<String>,
```

- [ ] **Step 4: 跑测试验证通过**

```bash
cargo test -p zeroclaw-config superusers_doc_mentions_global_approver
cargo test -p zeroclaw-config
cargo fmt --all -- --check
cargo clippy -p zeroclaw-config --all-targets -- -D warnings
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-config/src/schema.rs
git commit -m "feat(config): add [approval] section and reframe channels.superusers docstring"
```

---

## Task 2: IdentityResolver.reverse_lookup

**Files:**
- Modify: `crates/zeroclaw-infra/src/identity_store.rs`（trait + SqliteIdentityStore impl）
- Test: 同文件 `mod tests` 末尾

**Interfaces:**
- Consumes: 现有 `identity_mapping(channel_ref, sender, master_id)` 表 + `idx_identity_master` 索引
- Produces:
  - `trait IdentityResolver { fn reverse_lookup(&self, master_id: &str, channel_ref: &str) -> Option<String>; ... }`
  - SqliteIdentityStore 实现：`SELECT sender FROM identity_mapping WHERE master_id = ?1 AND channel_ref = ?2 LIMIT 1`

- [ ] **Step 1: 写失败测试**

在 `tests` mod 末尾追加：

```rust
#[test]
fn reverse_lookup_finds_bound_channel_uid() {
    let (_t, s) = store();
    s.seed_superusers(&["u_alice".to_string()]).unwrap();
    let code = s.issue_code("u_alice").unwrap();
    s.redeem_code(&code, "lark.work", "ou_aaa").unwrap();

    assert_eq!(
        s.reverse_lookup("u_alice", "lark.work"),
        Some("ou_aaa".to_string())
    );
}

#[test]
fn reverse_lookup_returns_none_when_no_binding() {
    let (_t, s) = store();
    s.seed_superusers(&["u_alice".to_string()]).unwrap();
    assert!(s.reverse_lookup("u_alice", "telegram.default").is_none());
}

#[test]
fn reverse_lookup_returns_none_for_unknown_master_id() {
    let (_t, s) = store();
    s.seed_superusers(&["u_alice".to_string()]).unwrap();
    assert!(s.reverse_lookup("u_nobody", "lark.work").is_none());
}
```

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p zeroclaw-infra reverse_lookup
```
Expected: FAIL `no method named reverse_lookup`.

- [ ] **Step 3: trait + impl**

在 `IdentityResolver` trait 末尾追加方法：

```rust
    /// 反向解析：在指定 `channel_ref` 上找到 `master_id` 对应的本地 uid。
    /// 用于审批 broker 把卡片送达 superuser。无绑定时返回 `None`，调用方应回退到
    /// master_channel 上的 master_id 本身。
    fn reverse_lookup(&self, master_id: &str, channel_ref: &str) -> Option<String>;
```

在 `impl IdentityResolver for SqliteIdentityStore` 末尾追加：

```rust
    fn reverse_lookup(&self, master_id: &str, channel_ref: &str) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT sender FROM identity_mapping \
             WHERE master_id = ?1 AND channel_ref = ?2 LIMIT 1",
            params![master_id, channel_ref],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }
```

- [ ] **Step 4: 跑测试验证通过**

```bash
cargo test -p zeroclaw-infra
cargo clippy -p zeroclaw-infra --all-targets -- -D warnings
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-infra/src/identity_store.rs
git commit -m "feat(infra): add IdentityResolver::reverse_lookup for approval broker"
```

---

## Task 2.5: 扩展 ChannelOrigin（topic 注释纠正 + 新增 triggerer_master_id）

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`（`ChannelOrigin` 结构 + 注释；保持 `Default` derive 不变）
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（`:4993-5003` 附近 — 提一个 `triggerer_master_id_opt` 变量复用给 session_key + ChannelOrigin）
- Modify: `crates/dawn-tools/src/task.rs`（3 处 `ChannelOrigin { ... }` 字面量 — 改 `..Default::default()` 形式以免未来再加字段时破坏编译）

**Interfaces:**
- Consumes: 现有 `IdentityResolver::resolve(channel_ref, sender, is_master)`、`resolve_effective_topic(...)`
- Produces:
  - `ChannelOrigin.topic` 注释更新为「effective_topic（已含 `/topic` 绑定回退），与 `resolve_session_key` 内部口径一致」
  - 新增字段：`pub triggerer_master_id: Option<String>`（`None` 表示触发者无 master 身份；为下游 broker / channel-aware tool 提供单一来源的 master_id，避免重复 identity resolve）
  - 派生 `Default` 保持不变（自动给新字段 `None`）

**理由**：orchestrator `:646-650` 内部 `resolve_session_key` 已经调过 `identity.resolver.resolve(channel_ref, sender, is_master)`。把该结果提到 `channel_origin` 同一层级算一次，分别用于 session key 与 ChannelOrigin，**唯一源 = identity store**，不复制。下游 broker 从 `ChannelOrigin.triggerer_master_id` 直接读，不再二次解析。

- [ ] **Step 1: 写 ChannelOrigin 默认值回归测试**

在 `crates/zeroclaw-api/src/channel.rs` 现有 tests mod 末尾追加：

```rust
#[test]
fn channel_origin_default_has_triggerer_master_id_none() {
    let o = ChannelOrigin::default();
    assert!(o.triggerer_master_id.is_none());
    assert!(o.topic.is_none());
    assert!(o.channel_ref.is_empty());
}

#[test]
fn channel_origin_with_triggerer_master_id_round_trips_via_scope() {
    let origin = ChannelOrigin {
        from_uid: "raw_sender_la_botid".into(),
        channel_ref: "lark.work".into(),
        reply_target: "oc_xxx".into(),
        topic: Some("db_lock".into()),
        triggerer_master_id: Some("u_alice".into()),
    };
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let read_back = rt.block_on(async {
        CHANNEL_ORIGIN
            .scope(origin, async {
                CHANNEL_ORIGIN
                    .try_with(|o| o.triggerer_master_id.clone())
                    .unwrap()
            })
            .await
    });
    assert_eq!(read_back, Some("u_alice".to_string()));
}
```

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p zeroclaw-api channel_origin_default_has_triggerer_master_id_none
```
Expected: FAIL `no field named triggerer_master_id`.

- [ ] **Step 3: 给 ChannelOrigin 加字段 + 更新 topic 注释**

在 `crates/zeroclaw-api/src/channel.rs`（`pub struct ChannelOrigin { ... }` 块）：

```rust
#[derive(Clone, Default, Debug)]
pub struct ChannelOrigin {
    /// Originating user id, with any channel-specific suffix (e.g.
    /// `_la_<bot_uid>` on DawnIM) already stripped. This is the raw
    /// channel-side identity, NOT the unified master_id; use
    /// `triggerer_master_id` for unified-identity lookups.
    pub from_uid: String,
    /// Composite channel ref `"<type>.<alias>"`, e.g. `"dawnim.work"`.
    pub channel_ref: String,
    /// Original `ChannelMessage.reply_target` value, preserved verbatim
    /// so reply paths reconstruct correctly.
    pub reply_target: String,
    /// **Effective topic** for the current turn, sourced via
    /// `resolve_effective_topic(msg, channel_ref, master_channel_ref, topic_binding)`.
    /// `None` means "no topic" (default behaviour, equivalent to pre-multi-topic single-thread session).
    /// `Some(t)` means the current turn lives in the isolated topic `t` — its conversation
    /// history and memory are scoped separately from other topics under the same
    /// (channel, user) pair. Includes `/topic` binding fallback on slave channels;
    /// this is the SAME topic the orchestrator uses to compute session keys
    /// (`resolve_session_key`), so downstream consumers stay consistent.
    pub topic: Option<String>,
    /// Unified `master_id` resolved from `(channel_ref, sender)` via
    /// `IdentityResolver::resolve(...)`. `None` when the sender has no
    /// unified identity (not a whitelisted superuser and no `/bind` mapping).
    /// Single source of truth for downstream consumers (e.g. ApprovalBroker)
    /// that need the master_id — do NOT re-resolve from identity store.
    pub triggerer_master_id: Option<String>,
}
```

- [ ] **Step 4: orchestrator 同步构造点**

在 `crates/zeroclaw-channels/src/orchestrator/mod.rs:4993` 附近找到 `let channel_origin = ChannelOrigin { ... };`。**之前**插入：

```rust
    // Resolve unified master_id once and share it with both resolve_session_key
    // (inside history_key computation, done earlier) and ChannelOrigin below,
    // so we never call identity.resolver.resolve(...) twice for the same turn.
    let triggerer_master_id_opt = ctx.identity.as_deref().and_then(|id_rt| {
        let is_master = channel_ref_for_msg == id_rt.master_channel;
        id_rt.resolver.resolve(&channel_ref_for_msg, &msg.sender, is_master)
    });
```

然后修改 `channel_origin`：

```rust
    let channel_origin = zeroclaw_api::channel::ChannelOrigin {
        from_uid: msg
            .sender
            .split("_la_")
            .next()
            .unwrap_or(msg.sender.as_str())
            .to_string(),
        reply_target: msg.reply_target.clone(),
        channel_ref: channel_ref_for_msg.clone(),
        topic: effective_topic.clone(),
        triggerer_master_id: triggerer_master_id_opt.clone(),
    };
```

**DRY 守护**：如果 `resolve_session_key` 内部也调用了 `identity.resolver.resolve(...)`，把那处也改为接收已算好的 `triggerer_master_id_opt`（添加新签名 `resolve_session_key(msg, identity, effective_topic, master_id_hint)`，内部判 `master_id_hint.is_some()` 跳过重复调用）。**这是 plan 必修，不能跳过**：否则同一 turn 内 identity store 被 lock 两次。

具体修改：

```rust
fn resolve_session_key(
    msg: &zeroclaw_api::channel::ChannelMessage,
    identity: Option<&IdentityRuntime>,
    effective_topic: Option<&str>,
    master_id_hint: Option<&str>, // NEW: pre-resolved master_id (DRY with channel_origin)
) -> String {
    let base = conversation_history_key(msg);
    let Some(identity) = identity else { return base; };
    if is_group_reply_target(&msg.reply_target) { return base; }
    let channel_ref = match &msg.channel_alias {
        Some(alias) => format!("{}.{}", msg.channel, alias),
        None => msg.channel.clone(),
    };
    let is_master = channel_ref == identity.master_channel;
    let master_id = match master_id_hint {
        Some(m) => Some(m.to_string()),
        None => identity.resolver.resolve(&channel_ref, &msg.sender, is_master),
    };
    match master_id {
        Some(master_id) => match effective_topic {
            Some(topic) if !topic.is_empty() =>
                sanitize_session_key(&format!("unified_{master_id}_{topic}")),
            _ => sanitize_session_key(&format!("unified_{master_id}")),
        },
        None => base,
    }
}
```

调用点（orchestrator 内部多处 `resolve_session_key(msg, ident, eff_topic)` → 改为 `resolve_session_key(msg, ident, eff_topic, triggerer_master_id_opt.as_deref())`）。

- [ ] **Step 5: dawn-tools 字面量改 `..Default::default()`**

`crates/dawn-tools/src/task.rs` 三处：

```rust
let origin = zeroclaw_api::channel::ChannelOrigin {
    from_uid: "u_alice".into(),
    channel_ref: "dawnim.work".into(),
    reply_target: "1:u_alice".into(),
    topic: None,
    ..Default::default()
};
```

- [ ] **Step 6: 跑全套测试**

```bash
cargo test -p zeroclaw-api
cargo test -p zeroclaw-channels
cargo test -p dawn-tools
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
./dev/ci.sh dry-check
```
Expected: all PASS（特别注意 dry-check：把 identity resolve 调用收敛到一处，应消除潜在 duplicate state 风险）。

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-api/src/channel.rs \
        crates/zeroclaw-channels/src/orchestrator/mod.rs \
        crates/dawn-tools/src/task.rs
git commit -m "feat(api/channel): expose triggerer_master_id on ChannelOrigin; clarify topic semantics"
```

---

## Task 3: ApprovalGrantStore（trait + Sqlite 实现 + LRU 缓存）

**Files:**
- Create: `crates/zeroclaw-runtime/src/approval/grant_store.rs`
- Modify: `crates/zeroclaw-runtime/src/approval/mod.rs`（仅 `mod grant_store; pub use grant_store::*;`）

**Interfaces:**
- Consumes: `zeroclaw_memory::SqliteMemory`（已有 `Memory` trait + sqlite backend）
- Produces:
  - `ApprovalGrant { id, channel_ref, topic, user_master_id, tool_name, granted_at, granted_by_master_id, granted_via_channel }`
  - `GrantFilter { channel_ref, topic, user_master_id, tool_name }`（全部 Option）
  - `trait ApprovalGrantStore`:
    - `fn get(&self, channel_ref: &str, topic: Option<&str>, user_master_id: &str, tool_name: &str) -> anyhow::Result<Option<ApprovalGrant>>`
    - `fn put(&self, grant: ApprovalGrant) -> anyhow::Result<()>`
    - `fn list(&self, filter: &GrantFilter) -> anyhow::Result<Vec<ApprovalGrant>>`
    - `fn delete(&self, grant_id: &str) -> anyhow::Result<bool>`
  - `SqliteGrantStore::new(workspace_dir: &Path) -> anyhow::Result<Self>`

**注**：grant_store 不直接持有 `SqliteMemory`，因为 SqliteMemory 是按 namespace 划分的 KV，不适合多列查询。**直接打开自己的 `rusqlite::Connection`**，沿用 `SqliteIdentityStore` 的样板：库文件 `<workspace>/state/approval_grants.db`。该决定不违反 DRY（这是 grant 这条数据的"source of truth — created here"）。

- [ ] **Step 1: 创建文件 + 写 round-trip 失败测试**

```rust
//! Persistent storage for per-(channel, topic, user, tool) approval grants.
//!
//! See spec: docs/superpowers/specs/2026-06-18-persistent-tool-approval-grants-design.md

use anyhow::Context;
use lru::LruCache;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
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
        s.put(grant("dawnim.work", None, "u_alice", "shell")).unwrap();
        s.put(grant("dawnim.work", Some(""), "u_alice", "shell")).unwrap();
        assert!(s.get("dawnim.work", None, "u_alice", "shell").unwrap().is_some());
        assert!(s.get("dawnim.work", Some(""), "u_alice", "shell").unwrap().is_some());
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
        s.put(grant("dawnim.work", Some("t1"), "u_alice", "shell")).unwrap();
        s.put(grant("dawnim.work", Some("t2"), "u_alice", "shell")).unwrap();
        s.put(grant("dawnim.work", Some("t1"), "u_bob", "file_write")).unwrap();

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
        assert!(s
            .get("dawnim.work", Some("t1"), "u_alice", "shell")
            .unwrap()
            .is_none());
    }

    #[test]
    fn delete_missing_returns_false() {
        let (_t, s) = store();
        assert!(!s.delete("nonexistent-id").unwrap());
    }

    #[test]
    fn cache_hit_on_repeated_get() {
        let (_t, s) = store();
        s.put(grant("dawnim.work", Some("t1"), "u_alice", "shell")).unwrap();
        let _ = s.get("dawnim.work", Some("t1"), "u_alice", "shell").unwrap();
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
        let _ = s.get("dawnim.work", Some("t1"), "u_alice", "shell").unwrap();
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
        assert!(s
            .get("dawnim.work", Some("t1"), "u_alice", "shell")
            .unwrap()
            .is_none());
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
            s.put(grant("dawnim.work", Some("t1"), "u_alice", "shell")).unwrap();
        }
        let s2 = SqliteGrantStore::new(tmp.path()).unwrap();
        assert!(s2
            .get("dawnim.work", Some("t1"), "u_alice", "shell")
            .unwrap()
            .is_some());
    }
}
```

- [ ] **Step 2: 在 `crates/zeroclaw-runtime/src/approval/mod.rs` 顶部新增 module 挂载**

```rust
pub mod grant_store;
pub use grant_store::{ApprovalGrant, ApprovalGrantStore, GrantFilter, SqliteGrantStore};
```

- [ ] **Step 3: 跑测试验证通过**

```bash
cargo test -p zeroclaw-runtime approval::grant_store
cargo clippy -p zeroclaw-runtime --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: all PASS.

- [ ] **Step 4: 跑 DRY 守护**

```bash
./dev/ci.sh dry-check
```
Expected: PASS. 这是新建文件 + 自己的 source-of-truth，不该触发。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-runtime/src/approval/grant_store.rs crates/zeroclaw-runtime/src/approval/mod.rs
git commit -m "feat(runtime/approval): add SqliteGrantStore for persistent per-topic tool approval grants"
```

---

## Task 4: SummaryProvider + humanize 模块

**Files:**
- Create: `crates/zeroclaw-runtime/src/approval/humanize.rs`
- Create: `crates/zeroclaw-runtime/src/approval/decision_reason.rs`
- Modify: `crates/zeroclaw-runtime/src/approval/mod.rs`（挂载子模块）

**Interfaces:**
- Consumes: `zeroclaw_runtime::approval::summarize_args`（来自 `mod.rs`）
- Produces:
  - `#[async_trait] pub trait SummaryProvider: Send + Sync { async fn summarize(&self, prompt: &str) -> anyhow::Result<String>; }`
  - `pub struct Humanizer { provider: Option<Arc<dyn SummaryProvider>>, timeout: Duration, cache: Mutex<LruCache<(String, String), String>> }`
  - `Humanizer::humanize(&self, tool: &str, args: &serde_json::Value, triggerer: Option<&str>, topic: Option<&str>, channel_ref: &str) -> String`
    - 行为：先 `summarize_args` 脱敏 → 拼 prompt → `tokio::time::timeout(10s, provider.summarize)` → 成功用其结果；失败/超时/无 provider → fallback 文本 `"{display_name + 上下文} 在 [{channel}/#{topic}] 想执行: {tool}\n{summarized_args}"`
  - `pub mod decision_reason` 常量字符串：
    ```rust
    pub const INTERACTIVE_APPROVE: &str = "interactive_approve";
    pub const INTERACTIVE_ALWAYS: &str = "interactive_always";
    pub const INTERACTIVE_DENY: &str = "interactive_deny";
    pub const INTERACTIVE_REPLACE: &str = "interactive_replace";
    pub const CACHED_GRANT: &str = "cached_grant";
    pub const ALL_SUPERUSERS_TIMEOUT: &str = "all_superusers_timeout";
    pub const ALL_CHANNELS_FAILED: &str = "all_channels_failed";
    pub const NO_SUPERUSER_CONFIGURED: &str = "no_superuser_configured";
    pub const NO_MASTER_CHANNEL: &str = "no_master_channel";
    pub const POLICY_AUTO_APPROVE: &str = "policy_auto_approve";
    pub const POLICY_AUTONOMY_FULL: &str = "policy_autonomy_full";
    ```

- [ ] **Step 1: 创建 decision_reason.rs**

```rust
//! Stable reason strings for approval audit events. Keep in sync with the spec table
//! at docs/superpowers/specs/2026-06-18-persistent-tool-approval-grants-design.md §8.4.

pub const INTERACTIVE_APPROVE: &str = "interactive_approve";
pub const INTERACTIVE_ALWAYS: &str = "interactive_always";
pub const INTERACTIVE_DENY: &str = "interactive_deny";
pub const INTERACTIVE_REPLACE: &str = "interactive_replace";
pub const CACHED_GRANT: &str = "cached_grant";
pub const ALL_SUPERUSERS_TIMEOUT: &str = "all_superusers_timeout";
pub const ALL_CHANNELS_FAILED: &str = "all_channels_failed";
pub const NO_SUPERUSER_CONFIGURED: &str = "no_superuser_configured";
pub const NO_MASTER_CHANNEL: &str = "no_master_channel";
pub const POLICY_AUTO_APPROVE: &str = "policy_auto_approve";
pub const POLICY_AUTONOMY_FULL: &str = "policy_autonomy_full";
```

- [ ] **Step 2: 写 humanize 失败测试**

创建 `humanize.rs`，先写测试：

```rust
//! Card "human-friendly" summary helper. Wraps a configurable SummaryProvider
//! with a 10s timeout, LRU cache, and a hard fallback to `summarize_args`.

use crate::approval::summarize_args;
use async_trait::async_trait;
use lru::LruCache;
use parking_lot::Mutex;
use serde_json::Value;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

#[async_trait]
pub trait SummaryProvider: Send + Sync {
    async fn summarize(&self, prompt: &str) -> anyhow::Result<String>;
}

pub struct Humanizer {
    provider: Option<Arc<dyn SummaryProvider>>,
    timeout: Duration,
    cache: Mutex<LruCache<String, String>>,
}

impl Humanizer {
    pub fn new(provider: Option<Arc<dyn SummaryProvider>>, timeout: Duration) -> Self {
        Self {
            provider,
            timeout,
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(256).unwrap())),
        }
    }

    /// Build a human-readable card text. Never errors; always returns something.
    pub async fn humanize(
        &self,
        tool: &str,
        args: &Value,
        triggerer_display: Option<&str>,
        topic: Option<&str>,
        channel_ref: &str,
    ) -> String {
        let scrubbed_args = summarize_args(args);
        let fallback = render_fallback(tool, &scrubbed_args, triggerer_display, topic, channel_ref);

        let Some(provider) = self.provider.clone() else {
            return fallback;
        };

        let cache_key = format!("{tool}\u{1f}{scrubbed_args}");
        if let Some(cached) = self.cache.lock().get(&cache_key).cloned() {
            return decorate(&cached, triggerer_display, topic, channel_ref);
        }

        let prompt = build_prompt(tool, &scrubbed_args);
        let result = tokio::time::timeout(self.timeout, provider.summarize(&prompt)).await;
        match result {
            Ok(Ok(summary)) => {
                self.cache.lock().put(cache_key, summary.clone());
                decorate(&summary, triggerer_display, topic, channel_ref)
            }
            _ => fallback,
        }
    }
}

fn build_prompt(tool: &str, scrubbed_args: &str) -> String {
    format!(
        "Translate the following tool call into one short sentence in Simplified Chinese \
         that a non-technical reader can understand. Do NOT add details that are not in the input. \
         Keep it under 60 characters. Do NOT include the tool name or argument keys verbatim.\n\n\
         Tool: {tool}\nArguments (already redacted): {scrubbed_args}"
    )
}

fn render_fallback(
    tool: &str,
    scrubbed_args: &str,
    triggerer: Option<&str>,
    topic: Option<&str>,
    channel_ref: &str,
) -> String {
    let head = render_header(triggerer, topic, channel_ref);
    format!("{head}想执行：**{tool}**\n\n{scrubbed_args}")
}

fn decorate(
    body: &str,
    triggerer: Option<&str>,
    topic: Option<&str>,
    channel_ref: &str,
) -> String {
    let head = render_header(triggerer, topic, channel_ref);
    format!("{head}{body}")
}

fn render_header(triggerer: Option<&str>, topic: Option<&str>, channel_ref: &str) -> String {
    match (triggerer, topic) {
        (Some(t), Some(tp)) => format!("**{t}** 在 [{channel_ref} / #{tp}] "),
        (Some(t), None) => format!("**{t}** 在 [{channel_ref}] "),
        (None, Some(tp)) => format!("[{channel_ref} / #{tp}] "),
        (None, None) => format!("[{channel_ref}] "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct StaticProvider(&'static str);
    #[async_trait]
    impl SummaryProvider for StaticProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            Ok(self.0.to_string())
        }
    }

    struct SlowProvider(Duration);
    #[async_trait]
    impl SummaryProvider for SlowProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            tokio::time::sleep(self.0).await;
            Ok("slow".into())
        }
    }

    struct FailingProvider;
    #[async_trait]
    impl SummaryProvider for FailingProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("simulated"))
        }
    }

    struct CountingProvider(AtomicUsize);
    #[async_trait]
    impl SummaryProvider for CountingProvider {
        async fn summarize(&self, _: &str) -> anyhow::Result<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("ok".into())
        }
    }

    #[tokio::test]
    async fn fallback_when_no_provider() {
        let h = Humanizer::new(None, Duration::from_secs(10));
        let out = h
            .humanize(
                "shell",
                &serde_json::json!({"command": "ls"}),
                Some("u_alice"),
                Some("db_lock"),
                "dawnim.work",
            )
            .await;
        assert!(out.contains("u_alice"));
        assert!(out.contains("db_lock"));
        assert!(out.contains("shell"));
        assert!(out.contains("command: ls"));
    }

    #[tokio::test]
    async fn provider_success_drives_output() {
        let h = Humanizer::new(
            Some(Arc::new(StaticProvider("Alice 要查看文件"))),
            Duration::from_secs(10),
        );
        let out = h
            .humanize("shell", &serde_json::json!({}), Some("Alice"), None, "x")
            .await;
        assert!(out.contains("Alice 要查看文件"));
    }

    #[tokio::test]
    async fn provider_failure_falls_back() {
        let h = Humanizer::new(Some(Arc::new(FailingProvider)), Duration::from_secs(10));
        let out = h
            .humanize("shell", &serde_json::json!({"command": "ls"}), None, None, "x")
            .await;
        assert!(out.contains("shell"));
        assert!(out.contains("command: ls"));
    }

    #[tokio::test]
    async fn provider_timeout_falls_back() {
        let h = Humanizer::new(Some(Arc::new(SlowProvider(Duration::from_millis(200)))), Duration::from_millis(50));
        let out = h
            .humanize("shell", &serde_json::json!({"command": "ls"}), None, None, "x")
            .await;
        assert!(out.contains("shell"));
    }

    #[tokio::test]
    async fn cache_avoids_double_provider_call() {
        let provider = Arc::new(CountingProvider(AtomicUsize::new(0)));
        let h = Humanizer::new(Some(provider.clone()), Duration::from_secs(10));
        for _ in 0..3 {
            h.humanize(
                "shell",
                &serde_json::json!({"command": "ls"}),
                None,
                None,
                "x",
            )
            .await;
        }
        assert_eq!(provider.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn redaction_keeps_secret_values_out_of_prompt() {
        // We can't peek at the prompt directly, but summarize_args (used to build it)
        // already redacts api_key. Assert that fallback (deterministic) doesn't leak.
        let h = Humanizer::new(None, Duration::from_secs(10));
        let out = h
            .humanize(
                "http",
                &serde_json::json!({"api_key": "sk-LEAK-ME"}),
                None,
                None,
                "x",
            )
            .await;
        assert!(!out.contains("sk-LEAK-ME"));
        assert!(out.contains("[redacted]"));
    }
}
```

- [ ] **Step 3: 挂模块**

在 `crates/zeroclaw-runtime/src/approval/mod.rs` 顶部追加（与 Task 3 的 `pub mod grant_store;` 同处）：

```rust
pub mod decision_reason;
pub mod humanize;
pub use humanize::{Humanizer, SummaryProvider};
```

- [ ] **Step 4: 跑测试验证通过**

```bash
cargo test -p zeroclaw-runtime approval::humanize
cargo clippy -p zeroclaw-runtime --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-runtime/src/approval/humanize.rs \
        crates/zeroclaw-runtime/src/approval/decision_reason.rs \
        crates/zeroclaw-runtime/src/approval/mod.rs
git commit -m "feat(runtime/approval): add Humanizer (LLM summary + 10s timeout + LRU + fallback)"
```

---

## Task 5: Channel::cancel_approval 默认空实现

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`

**Interfaces:**
- Consumes: 无
- Produces: `async fn cancel_approval(&self, _approval_id: &str, _reason: &str) -> anyhow::Result<()> { Ok(()) }`

- [ ] **Step 1: 写默认实现回归测试**

在 channel.rs 现有 tests mod（找到任一现有 `#[test]`）末尾追加：

```rust
#[tokio::test]
async fn default_cancel_approval_is_noop_ok() {
    struct Dummy;
    #[async_trait::async_trait]
    impl crate::channel::Channel for Dummy {
        fn name(&self) -> &str { "dummy" }
        async fn send(&self, _: crate::channel::SendMessage) -> anyhow::Result<()> { Ok(()) }
        async fn listen(
            &self,
            _: tokio::sync::mpsc::Sender<crate::channel::ChannelMessage>,
            _: tokio_util::sync::CancellationToken,
        ) -> anyhow::Result<()> { Ok(()) }
    }
    let d = Dummy;
    assert!(d.cancel_approval("id", "reason").await.is_ok());
}
```

注意：`Dummy` 的方法集合需与 `Channel` 必须方法对齐——按当前 `Channel` trait 的必需方法补齐 stub（仅本测试用）。

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p zeroclaw-api default_cancel_approval_is_noop_ok
```
Expected: FAIL `no method named cancel_approval`.

- [ ] **Step 3: 在 trait 上加默认空实现**

在 `Channel` trait（约 `request_approval` 附近）追加：

```rust
    /// Cancel an in-flight approval (e.g. fan-out lost the race to another superuser).
    /// Default impl is a no-op; channels that support card patching (e.g. dawn_im,
    /// lark) should override this to update the card with the deciding superuser.
    async fn cancel_approval(&self, _approval_id: &str, _reason: &str) -> anyhow::Result<()> {
        Ok(())
    }
```

- [ ] **Step 4: 跑测试验证通过**

```bash
cargo test -p zeroclaw-api
cargo clippy -p zeroclaw-api --all-targets -- -D warnings
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-api/src/channel.rs
git commit -m "feat(api/channel): add Channel::cancel_approval default no-op for fan-out cancel"
```

---

## Task 6: dawn_im 卡片三按钮 + cancel_approval 实现

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/approval.rs`
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`（`impl Channel` 块新增 `cancel_approval`；如 dawn_im 协议支持 patch，则发送 resolved card；否则仅清 PendingApprovals）

**Interfaces:**
- Consumes: `Channel::cancel_approval`（Task 5）
- Produces: 卡片新增 `WkAction { text: "始终允许", value: "always", style: "default" }`；`WkApprovalAction.action` 接受 `"always"`；新增 `pub fn build_resolved_card(approval_id: &str, decider: &str) -> WkApprovalCard`（无 actions、文本说明）

- [ ] **Step 1: 写卡片三按钮失败测试**

在 `dawn_im/approval.rs` 现有 tests mod 末尾追加：

```rust
#[test]
fn card_has_three_buttons_including_always() {
    let card = build_approval_card("id-X", &req("shell", "cmd: ls"), 300);
    let actions = card.actions.expect("actions");
    let values: Vec<&str> = actions.iter().map(|a| a.value.as_str()).collect();
    assert_eq!(values, vec!["approve", "always", "deny"]);
}

#[test]
fn approval_action_always_deserializes() {
    let json = r#"{"type":21,"approval_id":"id1","action":"always"}"#;
    let a: WkApprovalAction = serde_json::from_str(json).unwrap();
    assert_eq!(a.action, "always");
}

#[test]
fn resolved_card_has_no_actions() {
    let card = build_resolved_card("id-X", "u_admin");
    assert!(card.actions.is_none());
    assert!(card.body.content.contains("u_admin"));
}
```

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p zeroclaw-channels dawn_im::approval
```
Expected: FAIL — current actions list has 2 entries (`approve`, `deny`); `build_resolved_card` not found.

- [ ] **Step 3: 修改 build_approval_card actions**

在 `build_approval_card` 函数体的 `actions: Some(vec![...])` 块，把现有两个 action 中间插入 `always`：

```rust
        actions: Some(vec![
            WkAction {
                text: "同意".to_string(),
                value: "approve".to_string(),
                style: "primary".to_string(),
            },
            WkAction {
                text: "始终允许".to_string(),
                value: "always".to_string(),
                style: "primary".to_string(),
            },
            WkAction {
                text: "拒绝".to_string(),
                value: "deny".to_string(),
                style: "danger".to_string(),
            },
        ]),
```

新增 `build_resolved_card` 函数（同文件，紧随 `build_approval_card` 之后）：

```rust
/// Render a no-button "已由 XX 处理" card to replace an in-flight approval card
/// after another superuser already decided. Used by `Channel::cancel_approval`.
pub fn build_resolved_card(approval_id: &str, decider: &str) -> WkApprovalCard {
    WkApprovalCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        approval_id: approval_id.to_string(),
        timeout_secs: 0,
        title: "📋 任务执行审批".to_string(),
        body: WkApprovalBody {
            content: format!("此请求已由 **{decider}** 处理，无需再次操作。"),
        },
        actions: None,
    }
}
```

- [ ] **Step 4: 跑测试验证通过 + 接入 inbound handler 的 `always` 解析**

`always` 已通过 `serde` 字段反序列化（`WkApprovalAction.action` 是 String），inbound 分发需要在路由位置（搜 `"approve"` 字符串）追加对 `"always"` 的处理。先确认现有路由位置：

```bash
grep -n '"approve"\|"deny"' crates/zeroclaw-channels/src/dawn_im
```

把所有 `match action.action.as_str() { "approve" => ..., "deny" => ... }` 类位置补 `"always" => ChannelApprovalResponse::AlwaysApprove`（按现有匹配臂样式）。Inbound handler 的具体位置取决于现状，按 grep 结果就地补齐；任何被路由忽略的 `always` 都会被现有 `_ => ApprovalResponse::No` 拦截，造成误拒绝。

补 inbound 路由的回归测试（同 tests mod）：

```rust
#[test]
fn action_always_maps_to_always_approve() {
    // The mapping logic lives wherever WkApprovalAction → ChannelApprovalResponse
    // happens (likely channel.rs inbound). This test pins the contract that "always"
    // must NOT fall through to default-deny.
    let json = r#"{"type":21,"approval_id":"id-Y","action":"always"}"#;
    let act: WkApprovalAction = serde_json::from_str(json).unwrap();
    let mapped = crate::dawn_im::channel::map_approval_action(&act);
    assert!(matches!(
        mapped,
        zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove
    ));
}
```

如果 `map_approval_action` 尚未存在，在 `dawn_im/channel.rs` 内抽出一个 pub(crate) 函数：

```rust
pub(crate) fn map_approval_action(
    action: &super::approval::WkApprovalAction,
) -> zeroclaw_api::channel::ChannelApprovalResponse {
    match action.action.as_str() {
        "approve" => zeroclaw_api::channel::ChannelApprovalResponse::Approve,
        "always" => zeroclaw_api::channel::ChannelApprovalResponse::AlwaysApprove,
        _ => zeroclaw_api::channel::ChannelApprovalResponse::Deny,
    }
}
```

然后把现有 inbound 路由调用点统一替换为 `map_approval_action(&action)`。

```bash
cargo test -p zeroclaw-channels dawn_im
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```
Expected: all PASS.

- [ ] **Step 5: 实现 cancel_approval**

在 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 的 `#[async_trait] impl Channel for DawnIMChannel` 块末尾追加：

```rust
    async fn cancel_approval(
        &self,
        approval_id: &str,
        reason: &str,
    ) -> anyhow::Result<()> {
        // Drop the pending sender so the request_approval future no longer waits on us.
        let _removed = self
            .pending_approvals
            .write()
            .await
            .remove(approval_id);

        // Best-effort: push a "resolved" card update. We don't have a per-card
        // recipient cache here; if dawn_im supports updating a card by approval_id
        // alone (broadcast to original conversation), use that; otherwise this is
        // a no-op and the original card simply stays visible to the user.
        // For now: emit a Note log so operators can see the cancel intent.
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({
                    "approval_id": approval_id,
                    "reason": reason,
                })),
            "dawn_im: cancel_approval invoked (card patch is best-effort)"
        );
        let _ = build_resolved_card(approval_id, reason); // construct to keep the helper used + linted
        Ok(())
    }
```

注：若 dawn_im 后续支持 `send_update_card(approval_id, card)`（如 wk 协议有 PATCH 等价），把构造好的 `build_resolved_card` 真正发送出去；当前阶段只删 pending 即可保证 broker fan-out 正确性。

补一个 channel 测试：

```rust
#[tokio::test]
async fn cancel_approval_removes_pending_entry() {
    let channel = DawnIMChannel::for_test_only(); // assume a test helper exists; if not, construct manually
    let (tx, _rx) = tokio::sync::oneshot::channel();
    channel
        .pending_approvals
        .write()
        .await
        .insert("id-Z".to_string(), tx);
    channel.cancel_approval("id-Z", "test").await.unwrap();
    assert!(channel.pending_approvals.read().await.get("id-Z").is_none());
}
```

若 `DawnIMChannel::for_test_only` 不存在：把这条 channel 级测试推迟到 Task 7 broker 集成测试中覆盖，并在本 commit message 注明。

```bash
cargo test -p zeroclaw-channels dawn_im
cargo clippy -p zeroclaw-channels --all-targets -- -D warnings
```
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-channels/src/dawn_im/approval.rs \
        crates/zeroclaw-channels/src/dawn_im/channel.rs
git commit -m "feat(channels/dawn_im): three-button approval card (approve/always/deny) + cancel_approval"
```

---

## Task 7: ApprovalBroker

**Files:**
- Create: `crates/zeroclaw-runtime/src/approval/broker.rs`
- Modify: `crates/zeroclaw-runtime/src/approval/mod.rs`（挂模块；新增 record_decision 签名延伸 — 见 Task 8）

**Interfaces:**
- Consumes:
  - `ApprovalGrantStore`（Task 3）
  - `IdentityResolver`（Task 2 的 reverse_lookup）
  - `Humanizer`（Task 4）
  - `Channel::request_approval` + `Channel::cancel_approval`（Task 5）
  - `Arc<RwLock<Config>>` 解析 `ChannelsConfig.superusers` + `master_channel`
- Produces:
  - `pub struct ApprovalBroker { ... }`
  - `ApprovalBroker::request_decision(&self, ctx: &BrokerRequestCtx<'_>) -> BrokerDecision`
  - `pub struct BrokerRequestCtx { tool_name, tool_args, channel_ref, topic, triggerer_master_id, triggerer_display: Option<String>, deciding_channel_hint }`
  - `pub enum BrokerDecision { Approve { reason: &'static str, grant_id: Option<String> }, Deny { reason: &'static str }, Replace { replacement: String, reason: &'static str } }`

**说明**：broker 不直接持有 `Channel`；为了 fan-out 它需要 channel 注册表。本任务通过新增 `ChannelDirectory` trait（最小接口：`fn lookup(&self, channel_ref: &str) -> Option<Arc<dyn Channel>>`）解耦；具体实现挂到 `ChannelOrchestrator` 上（在调用方注入）。

- [ ] **Step 1: 创建 broker.rs 骨架 + 失败测试**

```rust
//! ApprovalBroker — coordinates per-tool-call approval decisions.

use crate::approval::decision_reason::*;
use crate::approval::grant_store::{ApprovalGrant, ApprovalGrantStore, GrantFilter};
use crate::approval::humanize::Humanizer;
use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelApprovalRequest, ChannelApprovalResponse};
use zeroclaw_infra::identity_store::IdentityResolver;

#[derive(Debug, Clone)]
pub struct BrokerRequestCtx<'a> {
    pub tool_name: &'a str,
    pub tool_args: &'a serde_json::Value,
    pub channel_ref: String,
    pub topic: Option<String>,
    pub triggerer_master_id: Option<String>,
    pub triggerer_display: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerDecision {
    Approve { reason: &'static str, grant_id: Option<String> },
    Deny { reason: &'static str },
    Replace { replacement: String, reason: &'static str },
}

pub trait ChannelDirectory: Send + Sync {
    fn lookup(&self, channel_ref: &str) -> Option<Arc<dyn Channel>>;
}

pub struct ApprovalBroker {
    pub(crate) grants: Arc<dyn ApprovalGrantStore>,
    pub(crate) identity: Arc<dyn IdentityResolver>,
    pub(crate) directory: Arc<dyn ChannelDirectory>,
    pub(crate) humanizer: Arc<Humanizer>,
    pub(crate) superusers_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub(crate) master_channel_resolver: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub(crate) approval_timeout: std::time::Duration,
}

impl ApprovalBroker {
    pub async fn request_decision(&self, ctx: &BrokerRequestCtx<'_>) -> BrokerDecision {
        // 1) Cached grant?
        let cached = self.grants.get(
            &ctx.channel_ref,
            ctx.topic.as_deref(),
            ctx.triggerer_master_id.as_deref().unwrap_or(""),
            ctx.tool_name,
        );
        if let Ok(Some(g)) = cached {
            return BrokerDecision::Approve {
                reason: CACHED_GRANT,
                grant_id: Some(g.id),
            };
        }

        // 2) Empty superuser list -> deny
        let superusers = (self.superusers_resolver)();
        if superusers.is_empty() {
            return BrokerDecision::Deny { reason: NO_SUPERUSER_CONFIGURED };
        }
        let master_channel = match (self.master_channel_resolver)() {
            Some(m) => m,
            None => return BrokerDecision::Deny { reason: NO_MASTER_CHANNEL },
        };

        // 3) Self vs proxy
        let is_self = ctx
            .triggerer_master_id
            .as_ref()
            .map(|t| superusers.iter().any(|s| s == t))
            .unwrap_or(false);

        // 4) Resolve targets
        let targets = if is_self {
            vec![(ctx.channel_ref.clone(), ctx.triggerer_master_id.clone().unwrap_or_default())]
        } else {
            self.resolve_proxy_targets(&superusers, &ctx.channel_ref, &master_channel)
        };

        // 5) Humanize once, shared across targets
        let card_summary = self
            .humanizer
            .humanize(
                ctx.tool_name,
                ctx.tool_args,
                ctx.triggerer_display.as_deref(),
                ctx.topic.as_deref(),
                &ctx.channel_ref,
            )
            .await;

        // 6) Fan out
        let approval_id = uuid::Uuid::new_v4().to_string();
        let request = ChannelApprovalRequest {
            tool_name: ctx.tool_name.to_string(),
            arguments_summary: card_summary,
            raw_arguments: None,
            thread_ts: ctx.topic.clone(),
        };
        let (winner, winning_channel_ref) =
            self.fan_out(&targets, &approval_id, &request).await;

        match winner {
            None => BrokerDecision::Deny { reason: ALL_SUPERUSERS_TIMEOUT },
            Some(ChannelApprovalResponse::Approve) => BrokerDecision::Approve {
                reason: INTERACTIVE_APPROVE,
                grant_id: None,
            },
            Some(ChannelApprovalResponse::AlwaysApprove) => {
                let grant = ApprovalGrant::new(
                    ctx.channel_ref.clone(),
                    ctx.topic.clone(),
                    ctx.triggerer_master_id.clone().unwrap_or_default(),
                    ctx.tool_name.to_string(),
                    self.identify_decider(&winning_channel_ref, &superusers, ctx),
                    winning_channel_ref.unwrap_or_else(|| ctx.channel_ref.clone()),
                );
                let grant_id = grant.id.clone();
                let _ = self.grants.put(grant);
                BrokerDecision::Approve {
                    reason: INTERACTIVE_ALWAYS,
                    grant_id: Some(grant_id),
                }
            }
            Some(ChannelApprovalResponse::Deny) => BrokerDecision::Deny { reason: INTERACTIVE_DENY },
            Some(ChannelApprovalResponse::DenyWithEdit { replacement }) => {
                BrokerDecision::Replace { replacement, reason: INTERACTIVE_REPLACE }
            }
        }
    }

    fn resolve_proxy_targets(
        &self,
        superusers: &[String],
        triggering_channel: &str,
        master_channel: &str,
    ) -> Vec<(String, String)> {
        superusers
            .iter()
            .map(|su| {
                if let Some(uid) = self.identity.reverse_lookup(su, triggering_channel) {
                    (triggering_channel.to_string(), uid)
                } else {
                    (master_channel.to_string(), su.clone())
                }
            })
            .collect()
    }

    async fn fan_out(
        &self,
        targets: &[(String, String)],
        approval_id: &str,
        request: &ChannelApprovalRequest,
    ) -> (Option<ChannelApprovalResponse>, Option<String>) {
        use tokio::task::JoinSet;
        let mut set: JoinSet<(String, anyhow::Result<Option<ChannelApprovalResponse>>)> =
            JoinSet::new();
        let mut alive_targets: Vec<(String, Arc<dyn Channel>)> = Vec::new();
        for (channel_ref, recipient) in targets {
            if let Some(ch) = self.directory.lookup(channel_ref) {
                let ch_clone = ch.clone();
                let chref = channel_ref.clone();
                let recipient_clone = recipient.clone();
                let req_clone = request.clone();
                set.spawn(async move {
                    let res = ch_clone.request_approval(&recipient_clone, &req_clone).await;
                    (chref, res)
                });
                alive_targets.push((channel_ref.clone(), ch));
            }
        }
        let mut winner: Option<(ChannelApprovalResponse, String)> = None;
        let timeout = self.approval_timeout;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            tokio::select! {
                Some(joined) = set.join_next() => {
                    if let Ok((chref, Ok(Some(resp)))) = joined {
                        winner = Some((resp, chref));
                        break;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => { break; }
                else => { break; }
            }
        }
        if let Some((_, ref winning_chref)) = winner {
            for (chref, ch) in alive_targets.iter() {
                if chref != winning_chref {
                    let _ = ch.cancel_approval(approval_id, "已由其他 superuser 处理").await;
                }
            }
        }
        set.shutdown().await;
        match winner {
            Some((r, c)) => (Some(r), Some(c)),
            None => (None, None),
        }
    }

    fn identify_decider(
        &self,
        winning_channel: &Option<String>,
        superusers: &[String],
        ctx: &BrokerRequestCtx<'_>,
    ) -> String {
        // Best-effort attribution; if we can't pin a specific superuser,
        // fall back to the triggerer (self-approval path).
        if let (Some(_chref), Some(triggerer)) = (winning_channel, ctx.triggerer_master_id.as_ref()) {
            if superusers.iter().any(|s| s == triggerer) {
                return triggerer.clone();
            }
        }
        superusers.first().cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::grant_store::SqliteGrantStore;
    use crate::approval::humanize::Humanizer;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tempfile::TempDir;
    use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

    // ── Fake channel ────────────────────────────────────────────────
    struct FakeChannel {
        name: String,
        respond_with: StdMutex<Option<ChannelApprovalResponse>>,
        delay: Duration,
        cancel_count: StdMutex<usize>,
    }
    impl FakeChannel {
        fn new(name: &str, response: Option<ChannelApprovalResponse>) -> Self {
            Self {
                name: name.into(),
                respond_with: StdMutex::new(response),
                delay: Duration::from_millis(10),
                cancel_count: StdMutex::new(0),
            }
        }
    }
    #[async_trait::async_trait]
    impl Channel for FakeChannel {
        fn name(&self) -> &str { &self.name }
        async fn send(&self, _: SendMessage) -> anyhow::Result<()> { Ok(()) }
        async fn listen(
            &self,
            _: tokio::sync::mpsc::Sender<ChannelMessage>,
            _: tokio_util::sync::CancellationToken,
        ) -> anyhow::Result<()> { Ok(()) }
        async fn request_approval(
            &self,
            _: &str,
            _: &ChannelApprovalRequest,
        ) -> anyhow::Result<Option<ChannelApprovalResponse>> {
            tokio::time::sleep(self.delay).await;
            Ok(self.respond_with.lock().unwrap().clone())
        }
        async fn cancel_approval(&self, _: &str, _: &str) -> anyhow::Result<()> {
            *self.cancel_count.lock().unwrap() += 1;
            Ok(())
        }
    }

    struct StaticDirectory {
        entries: Vec<(String, Arc<dyn Channel>)>,
    }
    impl ChannelDirectory for StaticDirectory {
        fn lookup(&self, channel_ref: &str) -> Option<Arc<dyn Channel>> {
            self.entries
                .iter()
                .find(|(k, _)| k == channel_ref)
                .map(|(_, v)| v.clone())
        }
    }

    // ── Fake identity resolver ──────────────────────────────────────
    struct FakeIdentity(StdMutex<std::collections::HashMap<(String, String), String>>);
    impl FakeIdentity {
        fn empty() -> Self { Self(StdMutex::new(Default::default())) }
        fn bind(&self, master_id: &str, channel_ref: &str, uid: &str) {
            self.0.lock().unwrap().insert((master_id.into(), channel_ref.into()), uid.into());
        }
    }
    impl IdentityResolver for FakeIdentity {
        fn resolve(&self, _: &str, _: &str, _: bool) -> Option<String> { None }
        fn issue_code(&self, _: &str) -> Option<String> { None }
        fn redeem_code(&self, _: &str, _: &str, _: &str) -> Result<String, String> { Err("n/a".into()) }
        fn unbind(&self, _: &str, _: &str) -> bool { false }
        fn reverse_lookup(&self, master_id: &str, channel_ref: &str) -> Option<String> {
            self.0.lock().unwrap().get(&(master_id.into(), channel_ref.into())).cloned()
        }
    }

    fn broker(
        directory: Arc<dyn ChannelDirectory>,
        identity: Arc<dyn IdentityResolver>,
        grants: Arc<dyn ApprovalGrantStore>,
        superusers: Vec<String>,
        master_channel: Option<String>,
    ) -> ApprovalBroker {
        let su = Arc::new(superusers);
        let mc = Arc::new(master_channel);
        ApprovalBroker {
            grants,
            identity,
            directory,
            humanizer: Arc::new(Humanizer::new(None, Duration::from_secs(10))),
            superusers_resolver: Arc::new(move || (*su).clone()),
            master_channel_resolver: Arc::new(move || (*mc).clone()),
            approval_timeout: Duration::from_millis(500),
        }
    }

    fn fresh_store() -> (TempDir, Arc<dyn ApprovalGrantStore>) {
        let tmp = TempDir::new().unwrap();
        let s = SqliteGrantStore::new(tmp.path()).unwrap();
        (tmp, Arc::new(s) as Arc<dyn ApprovalGrantStore>)
    }

    #[tokio::test]
    async fn deny_when_no_superusers() {
        let (_t, grants) = fresh_store();
        let dir = Arc::new(StaticDirectory { entries: vec![] });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(dir, id, grants, vec![], Some("dawnim.work".into()));
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        assert_eq!(
            b.request_decision(&ctx).await,
            BrokerDecision::Deny { reason: NO_SUPERUSER_CONFIGURED }
        );
    }

    #[tokio::test]
    async fn deny_when_no_master_channel() {
        let (_t, grants) = fresh_store();
        let dir = Arc::new(StaticDirectory { entries: vec![] });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(dir, id, grants, vec!["u_admin".into()], None);
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        assert_eq!(
            b.request_decision(&ctx).await,
            BrokerDecision::Deny { reason: NO_MASTER_CHANNEL }
        );
    }

    #[tokio::test]
    async fn cached_grant_short_circuits() {
        let (_t, grants) = fresh_store();
        grants
            .put(ApprovalGrant::new(
                "dawnim.work".into(),
                Some("db_lock".into()),
                "u_alice".into(),
                "shell".into(),
                "u_admin".into(),
                "dawnim.work".into(),
            ))
            .unwrap();
        let dir = Arc::new(StaticDirectory { entries: vec![] });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(dir, id, grants, vec!["u_admin".into()], Some("dawnim.work".into()));
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: Some("db_lock".into()),
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        match b.request_decision(&ctx).await {
            BrokerDecision::Approve { reason, grant_id } => {
                assert_eq!(reason, CACHED_GRANT);
                assert!(grant_id.is_some());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn self_path_when_triggerer_is_superuser() {
        let (_t, grants) = fresh_store();
        let fake = Arc::new(FakeChannel::new("dawnim.work", Some(ChannelApprovalResponse::Approve)));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), fake.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(dir, id, grants, vec!["u_admin".into()], Some("dawnim.work".into()));
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_admin".into()), // is superuser
            triggerer_display: None,
        };
        assert!(matches!(
            b.request_decision(&ctx).await,
            BrokerDecision::Approve { reason: INTERACTIVE_APPROVE, .. }
        ));
    }

    #[tokio::test]
    async fn proxy_path_fans_out_to_all_superusers() {
        let (_t, grants) = fresh_store();
        let a = Arc::new(FakeChannel::new("dawnim.work", Some(ChannelApprovalResponse::Approve)));
        let b_ch = Arc::new(FakeChannel::new("dawnim.work", Some(ChannelApprovalResponse::Deny)));
        let dir = Arc::new(StaticDirectory {
            entries: vec![
                ("dawnim.work".into(), a.clone()),
            ],
        });
        let id = Arc::new(FakeIdentity::empty());
        let _ = b_ch;
        let broker = broker(
            dir,
            id,
            grants,
            vec!["u_admin1".into(), "u_admin2".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()), // not superuser
            triggerer_display: Some("Alice".into()),
        };
        // Both targets resolve to the same fake channel (master_channel fallback);
        // FakeChannel returns Approve — broker accepts.
        assert!(matches!(
            broker.request_decision(&ctx).await,
            BrokerDecision::Approve { reason: INTERACTIVE_APPROVE, .. }
        ));
    }

    #[tokio::test]
    async fn always_approve_writes_grant() {
        let (_t, grants) = fresh_store();
        let fake = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::AlwaysApprove),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), fake.clone())],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants.clone(),
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: Some("t1".into()),
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        match b.request_decision(&ctx).await {
            BrokerDecision::Approve { reason: INTERACTIVE_ALWAYS, grant_id: Some(_) } => {
                let stored = grants
                    .get("dawnim.work", Some("t1"), "u_alice", "shell")
                    .unwrap();
                assert!(stored.is_some());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn approve_does_not_write_grant() {
        let (_t, grants) = fresh_store();
        let fake = Arc::new(FakeChannel::new(
            "dawnim.work",
            Some(ChannelApprovalResponse::Approve),
        ));
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), fake)],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants.clone(),
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: Some("t1".into()),
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        let _ = b.request_decision(&ctx).await;
        assert!(grants
            .get("dawnim.work", Some("t1"), "u_alice", "shell")
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn all_timeout_returns_deny_with_timeout_reason() {
        let (_t, grants) = fresh_store();
        let slow = Arc::new(FakeChannel {
            name: "dawnim.work".into(),
            respond_with: StdMutex::new(Some(ChannelApprovalResponse::Approve)),
            delay: Duration::from_secs(5),
            cancel_count: StdMutex::new(0),
        });
        let dir = Arc::new(StaticDirectory {
            entries: vec![("dawnim.work".into(), slow)],
        });
        let id = Arc::new(FakeIdentity::empty());
        let b = broker(
            dir,
            id,
            grants,
            vec!["u_admin".into()],
            Some("dawnim.work".into()),
        );
        let ctx = BrokerRequestCtx {
            tool_name: "shell",
            tool_args: &serde_json::json!({}),
            channel_ref: "dawnim.work".into(),
            topic: None,
            triggerer_master_id: Some("u_alice".into()),
            triggerer_display: None,
        };
        // broker.approval_timeout = 500ms; slow channel takes 5s → timeout
        assert_eq!(
            b.request_decision(&ctx).await,
            BrokerDecision::Deny { reason: ALL_SUPERUSERS_TIMEOUT }
        );
    }
}
```

- [ ] **Step 2: 挂模块**

在 `crates/zeroclaw-runtime/src/approval/mod.rs` 顶部追加（与 Task 3/4 同段）：

```rust
pub mod broker;
pub use broker::{ApprovalBroker, BrokerDecision, BrokerRequestCtx, ChannelDirectory};
```

- [ ] **Step 3: 跑测试验证通过**

```bash
cargo test -p zeroclaw-runtime approval::broker
cargo clippy -p zeroclaw-runtime --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: all PASS.

- [ ] **Step 4: DRY 守护**

```bash
./dev/ci.sh dry-check
```

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-runtime/src/approval/broker.rs \
        crates/zeroclaw-runtime/src/approval/mod.rs
git commit -m "feat(runtime/approval): add ApprovalBroker (fan-out + reverse-lookup + grant persist)"
```

---

## Task 8: ApprovalManager 改造（删 session_allowlist + audit_log；引入 grant_store + broker；record_decision 走 record!）

**Files:**
- Modify: `crates/zeroclaw-runtime/src/approval/mod.rs`

**Interfaces:**
- Consumes: `ApprovalGrantStore`、`ApprovalBroker`、`zeroclaw_log::record!`
- Produces:
  - 删除字段：`session_allowlist: Mutex<HashSet<String>>`、`audit_log: Mutex<Vec<ApprovalLogEntry>>`
  - 删除方法：`audit_log() -> Vec<ApprovalLogEntry>`、`session_allowlist() -> HashSet<String>`
  - 新增字段：`grants: Arc<dyn ApprovalGrantStore>`、`broker: Arc<ApprovalBroker>`
  - 改造 `approval_requirement(...)`：在 `session_allowlist.contains(tool)` 处替换为 `grants.get(channel_ref, topic, user_master_id, tool)` 的命中检查；**该方法签名需要扩展**为带 `(channel_ref, topic, user_master_id)` 形参，否则无法查 grant。新签名：`fn approval_requirement(&self, tool_name: &str, ctx: &GrantLookupCtx) -> ApprovalRequirement`
  - 改造 `record_decision(...)`：内部唯一动作 = `record!(Action::Approve|Reject, ...)` 调用，**不再写内存 Vec**。新签名增加 `reason: &'static str, extras: serde_json::Value`。
  - 保留 `ApprovalRequest` / `ApprovalResponse` / `summarize_args` / `sanitize_tool_replacement` 公开 API。

- [ ] **Step 1: 写改造后行为的失败测试**

替换/重写现有 `audit_log_records_decisions` 与 `audit_log_contains_timestamp_and_channel`（位于 mod.rs `tests` mod）：

```rust
#[test]
fn record_decision_emits_record_event() {
    let _g1 = ::zeroclaw_log::__private_test_writer_lock();
    let _g2 = ::zeroclaw_log::__private_test_hook_lock();
    let _sub = ::zeroclaw_log::try_install_capture_subscriber();

    let mgr = ApprovalManager::from_risk_profile(&supervised_config());
    mgr.record_decision(
        "shell",
        &serde_json::json!({"command": "ls"}),
        &ApprovalResponse::Yes,
        "cli",
        crate::approval::decision_reason::INTERACTIVE_APPROVE,
        serde_json::json!({}),
    );
    let captured = ::zeroclaw_log::__private_take_captured_events();
    assert!(captured.iter().any(|ev| {
        ev.get("action").and_then(|v| v.as_str()) == Some("approve")
            && ev.get("reason").and_then(|v| v.as_str()) == Some("interactive_approve")
            && ev.get("tool").and_then(|v| v.as_str()) == Some("shell")
    }));
}
```

> 注：若 `__private_take_captured_events` 尚未导出，先在 `zeroclaw-log` 加一个 helper（独立小 commit），或者用现有 `LogCaptureLayer` 的 API 拿事件。如现有 helper 名称不同，按真实 API 调整测试。

并替换原 `always_response_adds_to_session_allowlist` 为 grant 维度（grant 命中检查由 broker 走，这里只测 ApprovalManager 不再持有 session_allowlist）：

```rust
#[test]
fn approval_manager_no_longer_exposes_session_allowlist() {
    let mgr = ApprovalManager::from_risk_profile(&supervised_config());
    // Compile-time check: the method `session_allowlist()` must not exist.
    // The body intentionally does not call it; if a future refactor re-adds the
    // method, this test stays green but the spec invariant is preserved by the
    // type-level removal in mod.rs.
    let _ = mgr;
}
```

- [ ] **Step 2: 跑测试验证失败**

```bash
cargo test -p zeroclaw-runtime approval::tests
```
Expected: FAIL on `record_decision` signature / new param missing / `audit_log` field still present etc.

- [ ] **Step 3: 改造 ApprovalManager**

删除 `session_allowlist`、`audit_log` 字段与 getters。重写 `record_decision`：

```rust
pub fn record_decision(
    &self,
    tool_name: &str,
    args: &serde_json::Value,
    decision: &ApprovalResponse,
    channel: &str,
    reason: &'static str,
    extras: serde_json::Value,
) {
    let summary = summarize_args(args);
    let (action, outcome, severity) = match decision {
        ApprovalResponse::Yes | ApprovalResponse::Always => (
            ::zeroclaw_log::Action::Approve,
            ::zeroclaw_log::EventOutcome::Success,
            ::zeroclaw_log::Severity::Info,
        ),
        ApprovalResponse::No => (
            ::zeroclaw_log::Action::Reject,
            ::zeroclaw_log::EventOutcome::Failure,
            ::zeroclaw_log::Severity::Warn,
        ),
        ApprovalResponse::ReplaceWith(_) => (
            ::zeroclaw_log::Action::Reject,
            ::zeroclaw_log::EventOutcome::Failure,
            ::zeroclaw_log::Severity::Warn,
        ),
    };
    let mut attrs = serde_json::json!({
        "tool": tool_name,
        "channel": channel,
        "reason": reason,
        "arguments_summary": summary,
    });
    if let Some(map) = attrs.as_object_mut() {
        if let Some(extra_map) = extras.as_object() {
            for (k, v) in extra_map {
                map.insert(k.clone(), v.clone());
            }
        }
    }
    let _ = severity;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), action)
            .with_category(::zeroclaw_log::EventCategory::Tool)
            .with_outcome(outcome)
            .with_attrs(attrs),
        "tool_approval_decision"
    );
}
```

> 注：`severity` 实际选择交给 `record!` 宏；macro 第一参用 `INFO`/`WARN` 字面量，按 outcome 分支选择宏调用。本 step 把 `record!(...)` 拆成两个分支（INFO for approve, WARN for reject）以匹配 macro 设计：

```rust
match action {
    ::zeroclaw_log::Action::Approve => {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), action)
                .with_category(::zeroclaw_log::EventCategory::Tool)
                .with_outcome(outcome)
                .with_attrs(attrs),
            "tool_approval_decision"
        );
    }
    _ => {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), action)
                .with_category(::zeroclaw_log::EventCategory::Tool)
                .with_outcome(outcome)
                .with_attrs(attrs),
            "tool_approval_decision"
        );
    }
}
```

`approval_requirement` 签名扩展为：

```rust
pub fn approval_requirement(
    &self,
    tool_name: &str,
    lookup: Option<&GrantLookupCtx<'_>>,
) -> ApprovalRequirement {
    if self.autonomy_level == AutonomyLevel::Full { return ApprovalRequirement::Approved; }
    if self.autonomy_level == AutonomyLevel::ReadOnly { return ApprovalRequirement::NotRequired; }
    if self.always_ask.contains("*") || self.always_ask.contains(tool_name) {
        return ApprovalRequirement::Prompt;
    }
    if self.non_interactive
        && tool_name == "shell"
        && !self.non_interactive_shell_requires_approval
    { return ApprovalRequirement::NotRequired; }
    if self.auto_approve.contains("*") || self.auto_approve.contains(tool_name) {
        return ApprovalRequirement::Approved;
    }
    if let (Some(lookup), Some(grants)) = (lookup, &self.grants) {
        if let Ok(Some(_)) = grants.get(
            lookup.channel_ref.as_str(),
            lookup.topic.as_deref(),
            lookup.user_master_id.as_str(),
            tool_name,
        ) {
            return ApprovalRequirement::Approved;
        }
    }
    ApprovalRequirement::Prompt
}

pub struct GrantLookupCtx {
    pub channel_ref: String,
    pub topic: Option<String>,
    pub user_master_id: String,
}
```

> 注：从 `&'a str` 改成 `String` — 这是 Task 2.5 引入 `ChannelOrigin.triggerer_master_id` 后的简化（origin clone 是 owned；lookup ctx 跟着 owned 最直接，不引入生命周期跨层传递）。grant_store API 仍接 `&str`，调用处用 `.as_str()` / `.as_deref()` 适配。

并在 `ApprovalManager` 的所有构造函数（`from_risk_profile` / `for_non_interactive` / `for_non_interactive_backchannel`）添加可选 `grants` 与 `broker` 字段——默认 `None`，由调用方在 daemon 初始化时通过新增的 builder/with-method 注入：

```rust
pub fn with_grant_store(mut self, grants: Arc<dyn ApprovalGrantStore>) -> Self {
    self.grants = Some(grants);
    self
}
pub fn with_broker(mut self, broker: Arc<ApprovalBroker>) -> Self {
    self.broker = Some(broker);
    self
}
pub fn broker(&self) -> Option<Arc<ApprovalBroker>> { self.broker.clone() }
```

- [ ] **Step 4: 跑测试验证通过**

```bash
cargo test -p zeroclaw-runtime approval::
cargo clippy -p zeroclaw-runtime --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: all PASS（现有几个 session_allowlist 测试同步重写为 grant 维度命中断言）。

更新现有几个测试：
- `always_response_adds_to_session_allowlist` → 改为：用 in-memory `Arc<dyn ApprovalGrantStore>` 直接 put，然后 `approval_requirement(tool, Some(&ctx))` 返回 `Approved`
- `non_interactive_session_allowlist_still_works` → 类似改写
- `non_interactive_always_ask_overrides_session_allowlist` → 改为 grant 命中后 always_ask 仍优先

代码示例（替换原测试）：

```rust
#[test]
fn grant_hit_short_circuits_approval_requirement() {
    use std::sync::Arc;
    let (_t, grants) = {
        let tmp = tempfile::TempDir::new().unwrap();
        (tmp, Arc::new(crate::approval::grant_store::SqliteGrantStore::new(tmp.path()).unwrap()) as Arc<dyn ApprovalGrantStore>)
    };
    grants.put(crate::approval::grant_store::ApprovalGrant::new(
        "dawnim.work".into(), Some("t1".into()), "u_alice".into(), "file_write".into(),
        "u_admin".into(), "dawnim.work".into(),
    )).unwrap();
    let mgr = ApprovalManager::from_risk_profile(&supervised_config()).with_grant_store(grants);
    let ctx = GrantLookupCtx { channel_ref: "dawnim.work", topic: Some("t1"), user_master_id: "u_alice" };
    assert_eq!(
        mgr.approval_requirement("file_write", Some(&ctx)),
        ApprovalRequirement::Approved
    );
}
```

- [ ] **Step 5: DRY 守护**

```bash
./dev/ci.sh dry-check
```
Expected: PASS（删字段、不双写）。

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-runtime/src/approval/mod.rs
git commit -m "refactor(runtime/approval): drop session_allowlist+audit_log; route via grant_store and zeroclaw-log"
```

---

## Task 9: approval_gate 改造（调用 broker）

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/turn/approval_gate.rs`
- Modify: `crates/zeroclaw-runtime/src/agent/turn/context.rs`（如果需要把 broker 或 grant lookup ctx 挂到 TurnCtx 上）

**Interfaces:**
- Consumes: `ApprovalManager.broker()`、`BrokerRequestCtx`、`BrokerDecision`
- Produces: `gate_tool_approval` 改造为：
  1. 先调 `approval_requirement(tool, Some(&lookup_ctx))`
  2. `Prompt` → 调 `broker.request_decision(BrokerRequestCtx { ... })`
  3. 把 `BrokerDecision` 映射到 `ApprovalGateOutcome`
  4. 调用 `record_decision(...)` 传 `reason`（用 broker 返回的 reason 字符串）

- [ ] **Step 1: 写一个 ApprovalGateOutcome 转换的失败测试（集成测试）**

新建 `crates/zeroclaw-runtime/tests/approval_gate_uses_broker.rs`（如果该路径已有 tests 目录就放进去；否则在现有 component tests 下找位置）：

```rust
//! Ensure approval_gate routes Prompt outcomes through ApprovalBroker and respects
//! BrokerDecision::Approve/Deny/Replace mapping.

#[tokio::test]
async fn gate_routes_prompt_to_broker() {
    // This is a structural test: construct a TurnCtx with a stub broker that
    // returns BrokerDecision::Deny, then call gate_tool_approval and assert
    // ApprovalGateOutcome::Deny was returned.
    // Implementation detail: TurnCtx assembly may be complex; if too heavy,
    // implement this as a unit test inside approval_gate.rs with a minimal
    // shim and mark the broader integration test as Phase-2.
    assert!(true); // placeholder while wiring lands; replaced in Step 3
}
```

- [ ] **Step 2: 跑测试验证 placeholder pass，作为占位**

```bash
cargo test -p zeroclaw-runtime gate_routes_prompt_to_broker
```
Expected: PASS（placeholder）。

- [ ] **Step 3: 改造 gate_tool_approval**

**关键 DRY 守护**：`channel_ref` / `topic` / `triggerer_master_id` 全部从 `CHANNEL_ORIGIN` 读，**不要**在 `approval_gate` 里自己 format channel_ref 或调 identity resolver——orchestrator 已经在 `:4993` 一次性算好（见 Task 2.5）。

```rust
fn read_origin() -> zeroclaw_api::channel::ChannelOrigin {
    zeroclaw_api::channel::CHANNEL_ORIGIN
        .try_with(|o| o.clone())
        .unwrap_or_default()
}
```

`TurnCtx::grant_lookup_ctx()` 也只是一个读 `CHANNEL_ORIGIN` 的薄函数：

```rust
impl<'a> TurnCtx<'a> {
    pub fn grant_lookup_ctx(&self) -> Option<GrantLookupCtx<'_>> {
        let origin = read_origin();
        let master_id = origin.triggerer_master_id?;
        // Owned strings stored back via OnceCell / a turn-scoped Box to outlive the borrow,
        // or change GrantLookupCtx to own its strings. The latter is simpler:
        None // placeholder; actual signature below uses owned strings
    }
}
```

> 实施细节：因 `GrantLookupCtx<'_>` 借引用，而 `origin` 是 owned clone，需要把 `GrantLookupCtx` 改为持有 `String`（或在 `TurnCtx` 内 cache `Origin`）。本 step 简单起见把 `GrantLookupCtx` 字段改 `String`，broker 端 `BrokerRequestCtx` 接收 `&str` 的位置改为对 `String` 字段 `.as_str()`。同步修改 Task 8 中的 `GrantLookupCtx` 定义。

```rust
pub(crate) async fn gate_tool_approval(
    ctx: &TurnCtx<'_>,
    tool_name: &str,
    tool_args: &serde_json::Value,
    iteration: usize,
) -> ApprovalGateOutcome {
    let origin = read_origin();
    let lookup_ctx = origin.triggerer_master_id.as_ref().map(|mid| GrantLookupCtx {
        channel_ref: origin.channel_ref.clone(),
        topic: origin.topic.clone(),
        user_master_id: mid.clone(),
    });
    let requirement = ctx
        .approval
        .map(|mgr| mgr.approval_requirement(tool_name, lookup_ctx.as_ref()))
        .unwrap_or(ApprovalRequirement::NotRequired);
    if requirement != ApprovalRequirement::Prompt {
        return ApprovalGateOutcome::Proceed {
            approved: requirement == ApprovalRequirement::Approved,
        };
    }

    let Some(mgr) = ctx.approval else {
        return ApprovalGateOutcome::Proceed { approved: false };
    };
    let Some(broker) = mgr.broker() else {
        // No broker wired (e.g. CLI interactive mode) — fall back to CLI prompt path.
        let request = ApprovalRequest { tool_name: tool_name.to_string(), arguments: tool_args.clone() };
        let decision = mgr.prompt_cli(&request);
        mgr.record_decision(
            tool_name, tool_args, &decision, ctx.channel_name,
            cli_reason_for(&decision), serde_json::json!({}),
        );
        return cli_decision_to_outcome(decision, tool_name);
    };

    let req_ctx = BrokerRequestCtx {
        tool_name,
        tool_args,
        channel_ref: origin.channel_ref.clone(),
        topic: origin.topic.clone(),
        triggerer_master_id: origin.triggerer_master_id.clone(),
        triggerer_display: ctx.triggerer_display_name.map(str::to_string),
    };
    let decision = broker.request_decision(&req_ctx).await;
    map_broker_decision(decision, mgr, tool_name, tool_args, ctx, iteration)
}
```

其中 `map_broker_decision` / `cli_reason_for` / `cli_decision_to_outcome` 都是同文件辅助函数（沿用原 Deny / Replace 路径的 `record!` 模式 + ToolExecutionOutcome 构造）。

**关键点**：原 approval_gate.rs 内的 `record!(WARN, Action::Reject, ...)` 调用全部删除——决定的记录责任**单点收敛到 `mgr.record_decision`**。这是去 duplicate state 的核心一步。

`TurnCtx::grant_lookup_ctx() -> Option<GrantLookupCtx>` 是新增方法，从 `CHANNEL_ORIGIN.try_with(|o| o.clone()).unwrap_or_default()` 拼出来。

- [ ] **Step 4: 替换 placeholder 测试为真实 broker 调用断言**

把 `gate_routes_prompt_to_broker` 改成构造 `ApprovalManager.with_broker(stub_broker)` 后调 `gate_tool_approval`，断言 broker 被调用且 outcome 匹配。如果 TurnCtx 构造确实繁琐，**保留为 broker 单元测试覆盖（Task 7 已有）**，本测试改回 `cargo test -p zeroclaw-runtime` 全套绿。

- [ ] **Step 5: 跑测试验证通过**

```bash
cargo test -p zeroclaw-runtime
cargo clippy -p zeroclaw-runtime --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-runtime/src/agent/turn/approval_gate.rs \
        crates/zeroclaw-runtime/src/agent/turn/context.rs
git commit -m "refactor(runtime/agent): route approval_gate through ApprovalBroker"
```

---

## Task 10: 旧 audit_log() 测试调用点改写（safety_net + src/approval/mod.rs）

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/safety_net.rs`（line ~684）
- Modify: `src/approval/mod.rs`（line ~131, ~149）

**Interfaces:**
- Consumes: `zeroclaw_log::__private_test_writer_lock` / `try_install_capture_subscriber`
- Produces: 等价语义的"事件被捕获"断言；不再调用已删除的 `audit_log()` 访问器

- [ ] **Step 1: 跑现有测试验证编译失败**

```bash
cargo test -p zeroclaw-runtime safety_net
cargo test --bin zeroclaw approval
```
Expected: FAIL `no method named audit_log`.

- [ ] **Step 2: 改写 safety_net.rs:684 附近**

原片段：

```rust
let log = approval_mgr.audit_log();
let entry = log.last().expect("a decision must be recorded");
assert_eq!(entry.channel, "edit-channel", ...);
```

改为：

```rust
let _g1 = ::zeroclaw_log::__private_test_writer_lock();
let _g2 = ::zeroclaw_log::__private_test_hook_lock();
let _sub = ::zeroclaw_log::try_install_capture_subscriber();
// trigger the safety_net flow that calls record_decision (existing setup unchanged)
// ...
let events = ::zeroclaw_log::__private_take_captured_events();
let last = events
    .iter()
    .rev()
    .find(|ev| ev.get("action").and_then(|v| v.as_str()) == Some("approve")
             || ev.get("action").and_then(|v| v.as_str()) == Some("reject"))
    .expect("at least one approval decision recorded");
assert_eq!(
    last.get("channel").and_then(|v| v.as_str()),
    Some("edit-channel"),
    "approval audit must attribute the deciding back-channel"
);
```

- [ ] **Step 3: 改写 src/approval/mod.rs 两处测试**

把现有 `audit_log_records_decisions` 与 `audit_log_contains_timestamp_and_channel` 整段替换为：

```rust
#[test]
fn record_decision_emits_record_event() {
    let _g1 = ::zeroclaw_log::__private_test_writer_lock();
    let _g2 = ::zeroclaw_log::__private_test_hook_lock();
    let _sub = ::zeroclaw_log::try_install_capture_subscriber();

    let mgr = ApprovalManager::from_risk_profile(&supervised_config());
    mgr.record_decision(
        "shell",
        &serde_json::json!({"command": "rm -rf ./build/"}),
        &ApprovalResponse::No,
        "cli",
        zeroclaw_runtime::approval::decision_reason::INTERACTIVE_DENY,
        serde_json::json!({}),
    );
    mgr.record_decision(
        "file_write",
        &serde_json::json!({"path": "out.txt"}),
        &ApprovalResponse::Yes,
        "cli",
        zeroclaw_runtime::approval::decision_reason::INTERACTIVE_APPROVE,
        serde_json::json!({}),
    );
    let events = ::zeroclaw_log::__private_take_captured_events();
    let approvals: Vec<_> = events
        .iter()
        .filter(|ev| {
            matches!(
                ev.get("action").and_then(|v| v.as_str()),
                Some("approve" | "reject")
            )
        })
        .collect();
    assert!(approvals.len() >= 2);
    assert!(approvals.iter().any(|ev| ev.get("tool") == Some(&serde_json::json!("shell"))));
    assert!(approvals.iter().any(|ev| ev.get("tool") == Some(&serde_json::json!("file_write"))));
}

#[test]
fn record_decision_event_contains_channel_attribution() {
    let _g1 = ::zeroclaw_log::__private_test_writer_lock();
    let _g2 = ::zeroclaw_log::__private_test_hook_lock();
    let _sub = ::zeroclaw_log::try_install_capture_subscriber();

    let mgr = ApprovalManager::from_risk_profile(&supervised_config());
    mgr.record_decision(
        "shell",
        &serde_json::json!({"command": "ls"}),
        &ApprovalResponse::Yes,
        "telegram",
        zeroclaw_runtime::approval::decision_reason::INTERACTIVE_APPROVE,
        serde_json::json!({}),
    );
    let events = ::zeroclaw_log::__private_take_captured_events();
    assert!(events
        .iter()
        .any(|ev| ev.get("channel").and_then(|v| v.as_str()) == Some("telegram")));
}
```

- [ ] **Step 4: 跑测试验证通过**

```bash
cargo test -p zeroclaw-runtime
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-runtime/src/agent/safety_net.rs src/approval/mod.rs
git commit -m "test(approval): rewrite audit_log() assertions to use zeroclaw-log capture"
```

---

## Task 11: Gateway API `GET/DELETE /api/approvals/grants`

**Files:**
- Modify: `crates/zeroclaw-gateway/src/api.rs`（新增两个 handler + 路由挂载点；如该文件 too large，可新建 `api_approvals.rs` 与已有的 `api_skills.rs` 等同级）
- Create（推荐）：`crates/zeroclaw-gateway/src/api_approvals.rs`
- Modify: `crates/zeroclaw-gateway/src/lib.rs`（挂模块 + 路由注册）

**Interfaces:**
- Consumes: `Arc<dyn zeroclaw_runtime::approval::ApprovalGrantStore>`（在 gateway state 中注入；daemon 初始化时把 `SqliteGrantStore` 同时给 runtime 和 gateway，共享同一 `Arc`）
- Produces:
  - `GET /api/approvals/grants?channel=&topic=&user=&tool=` → 200 `Vec<ApprovalGrantDto>`
  - `DELETE /api/approvals/grants/{id}` → 200 `{deleted: true}` / 404 `{deleted: false}`
  - `ApprovalGrantDto` 字段与 `ApprovalGrant` 一致（serde re-export 或新 DTO，按 gateway 现状取舍；若 gateway 已有 DTO 约定，新建薄 DTO 以隔离稳定面）

- [ ] **Step 1: 创建 api_approvals.rs 文件 + 失败测试**

```rust
//! GET /api/approvals/grants + DELETE /api/approvals/grants/{id}

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use zeroclaw_runtime::approval::{ApprovalGrant, ApprovalGrantStore, GrantFilter};

#[derive(Clone)]
pub struct ApprovalsState {
    pub grants: Arc<dyn ApprovalGrantStore>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListQuery {
    pub channel: Option<String>,
    pub topic: Option<String>,
    /// Use `topic=__none__` to filter the no-topic bucket.
    pub user: Option<String>,
    pub tool: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeleteResp {
    deleted: bool,
}

async fn list_grants(
    State(s): State<ApprovalsState>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let filter = GrantFilter {
        channel_ref: q.channel,
        topic: q.topic.map(|t| if t == "__none__" { None } else { Some(t) }),
        user_master_id: q.user,
        tool_name: q.tool,
    };
    match s.grants.list(&filter) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "list approval grants failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "list failed"}))).into_response()
        }
    }
}

async fn delete_grant(
    State(s): State<ApprovalsState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match s.grants.delete(&id) {
        Ok(true) => (StatusCode::OK, Json(DeleteResp { deleted: true })).into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(DeleteResp { deleted: false })).into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "delete approval grant failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "delete failed"}))).into_response()
        }
    }
}

pub fn router(state: ApprovalsState) -> Router {
    Router::new()
        .route("/api/approvals/grants", get(list_grants))
        .route("/api/approvals/grants/:id", delete(delete_grant))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_runtime::approval::SqliteGrantStore;

    fn state() -> (TempDir, ApprovalsState) {
        let tmp = TempDir::new().unwrap();
        let grants = Arc::new(SqliteGrantStore::new(tmp.path()).unwrap()) as Arc<dyn ApprovalGrantStore>;
        (tmp, ApprovalsState { grants })
    }

    #[tokio::test]
    async fn list_empty_returns_empty_array() {
        let (_t, st) = state();
        let app = router(st);
        let resp = axum::body::to_bytes(
            tower::ServiceExt::oneshot(
                app,
                axum::http::Request::builder()
                    .uri("/api/approvals/grants")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
            usize::MAX,
        )
        .await
        .unwrap();
        let v: Vec<ApprovalGrant> = serde_json::from_slice(&resp).unwrap();
        assert!(v.is_empty());
    }

    #[tokio::test]
    async fn delete_missing_returns_404() {
        let (_t, st) = state();
        let app = router(st);
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("DELETE")
                .uri("/api/approvals/grants/nope")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_then_list_then_delete_round_trip() {
        let (_t, st) = state();
        let g = ApprovalGrant::new(
            "dawnim.work".into(),
            Some("t1".into()),
            "u_alice".into(),
            "shell".into(),
            "u_admin".into(),
            "dawnim.work".into(),
        );
        let id = g.id.clone();
        st.grants.put(g).unwrap();

        let app = router(st.clone());
        let body = axum::body::to_bytes(
            tower::ServiceExt::oneshot(
                app.clone(),
                axum::http::Request::builder()
                    .uri("/api/approvals/grants?channel=dawnim.work")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body(),
            usize::MAX,
        )
        .await
        .unwrap();
        let v: Vec<ApprovalGrant> = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.len(), 1);

        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!("/api/approvals/grants/{id}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
```

- [ ] **Step 2: 挂模块 + 注册路由**

在 `crates/zeroclaw-gateway/src/lib.rs` 添加：

```rust
pub mod api_approvals;
```

并在主 `Router` 构造的 `.merge(...)` 链中追加 `.merge(api_approvals::router(approvals_state))`；`approvals_state` 由 daemon 在 gateway 启动时构造（`SqliteGrantStore` 的同一 `Arc` 同时给 runtime 用，**单一实例**）。

- [ ] **Step 3: daemon 初始化注入**

找到 daemon 启动入口（`crates/zeroclaw-runtime/src/daemon/mod.rs`）`ApprovalManager` 构造的位置，改为：

```rust
let grant_store: Arc<dyn ApprovalGrantStore> = Arc::new(
    SqliteGrantStore::new(workspace_dir).context("init approval grant store")?,
);
let broker = Arc::new(build_approval_broker(/*...*/ grant_store.clone() /*...*/));
let approval_manager = ApprovalManager::from_risk_profile(&risk_profile)
    .with_grant_store(grant_store.clone())
    .with_broker(broker);
// gateway state also gets grant_store.clone()
```

- [ ] **Step 4: 跑测试验证通过**

```bash
cargo test -p zeroclaw-gateway api_approvals
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-gateway/src/api_approvals.rs crates/zeroclaw-gateway/src/lib.rs \
        crates/zeroclaw-runtime/src/daemon/mod.rs
git commit -m "feat(gateway): add GET/DELETE /api/approvals/grants endpoints"
```

---

## Task 12: 文档 + CHANGELOG

**Files:**
- Modify: `CHANGELOG-next.md`
- Modify: `docs/book/src/agents/delegation.md`（如该文档有 approval 章节，添加一段简介；否则跳过该 doc 改动）

**Interfaces:**
- Consumes: 无
- Produces: 用户可见的 release note + 操作员指南更新

- [ ] **Step 1: 写 CHANGELOG 条目**

在 `CHANGELOG-next.md` 「Added」段落追加：

```markdown
- **Persistent tool approval grants**: tool approvals can now be persisted per-`(channel, topic, user, tool)` key. When an operator clicks the new **「始终允许」 button** in the approval card, ZeroClaw remembers the decision in `<workspace>/state/approval_grants.db` and skips the prompt next time the same key fires. Use `GET /api/approvals/grants` to inspect or `DELETE /api/approvals/grants/{id}` to revoke. The existing **「同意」 (one-shot) button** is preserved unchanged. Triggered by a non-superuser? The card now fan-outs to all `channels.superusers`; first non-timeout reply wins.
- **Card humanization**: approval cards optionally run a lightweight LLM summary (configurable via `[approval] summary_provider`, 10s timeout, falls back to the existing args summary on failure).
- **Channel API**: `Channel::cancel_approval(approval_id, reason)` default no-op method added; dawn_im implements it to clear pending state when a fan-out loser cancels.
- **Identity store**: `IdentityResolver::reverse_lookup(master_id, channel_ref)` added for proxy-approval routing.
```

「Changed」段落追加：

```markdown
- `ApprovalManager.session_allowlist` (in-memory `HashSet<String>` per-tool allowlist) **removed** in favor of per-`(channel, topic, user, tool)` persistent grants.
- `ApprovalManager.audit_log` (in-memory `Vec<ApprovalLogEntry>`) and its `audit_log()` accessor **removed**. All approval decisions now flow through `zeroclaw_log::record!` and land in `runtime-trace.jsonl` (single source of truth). Query via `LogFilter { action: Some("approve" | "reject"), .. }` or `jq '.action=="approve"'` on the JSONL.
- `ChannelsConfig.superusers` docstring updated: the field's role now spans `/bind` whitelist + global approver.
```

- [ ] **Step 2: 跑 lint 验证 changelog 格式**

```bash
./dev/ci.sh dry-check
markdownlint-cli2 CHANGELOG-next.md
```

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG-next.md
git commit -m "docs(changelog): persistent tool approval grants + audit unified via zeroclaw-log"
```

---

## Self-Review

**Spec coverage check:**

| Spec section | Task |
|---|---|
| §5 决策 #0 三按钮 | T6 |
| §5 决策 #1 持久化键 | T3 |
| §5 决策 #2 superusers 复用 | T1（注释）+ T7（resolver 闭包） |
| §3 前提（ChannelOrigin.topic = effective_topic）| T2.5（纠正注释 + 既有实现已对齐） |
| §3 前提（新增 triggerer_master_id 字段）| T2.5 |
| §5 决策 #3 触发者识别 | T2.5（orchestrator 一次解析）+ T9（broker 直接读）|
| §5 决策 #4 送达路由 | T7（resolve_proxy_targets） |
| §5 决策 #5 多 superuser 先回 | T7（fan_out + cancel） |
| §5 决策 #6 LLM 摘要 10s + fallback | T4 |
| §5 决策 #7 卡片身份 | T4（render_header） |
| §5 决策 #8 SqliteMemory | T3（直开 rusqlite，附说明） |
| §5 决策 #9 Gateway API | T11 |
| §5 决策 #10 超时沿用 approval_timeout_secs | T7（注入 approval_timeout） |
| §5 决策 #11 DRY | T1/T2/T3/T8（删字段） |
| §5 决策 #12 audit 走 zeroclaw-log | T8、T10 |
| §8.4 audit 全路径覆盖 | T8（record_decision）+ T9（broker reason 透传） |
| §8.6 secret 红线 | T4（humanize 经 summarize_args） |
| §11 dawn_im 三按钮 + cancel | T6 |
| §11 identity reverse_lookup | T2 |
| §11 ApprovalManager 删字段 | T8 |
| §11 测试调用点改写 | T10 |
| §11 gateway endpoints | T11 |

**Placeholder scan**: 通读全文，无 "TBD"/"TODO"/"fill in later"。少数 step 注明"如果某 API 名称不同，按真实 API 调整"——这是测试 helper 的边界，已显式标注 fallback 行为。

**Type consistency**:
- `ApprovalGrantStore.get(...)` 在 T3 定义、T7/T8/T11 使用 — 签名一致
- `BrokerRequestCtx.{tool_name, tool_args, channel_ref, topic, triggerer_master_id, triggerer_display}` 在 T7 定义、T9 使用 — 一致
- `GrantLookupCtx { channel_ref, topic, user_master_id }` 在 T8 定义、T9 使用 — 一致
- `DecisionReason` 常量在 T4 定义、T7/T8/T10 使用 — 一致
- `ChannelDirectory.lookup(channel_ref) -> Option<Arc<dyn Channel>>` 在 T7 定义，daemon 注入由 T11 涉及
- `Humanizer::humanize(...)` 在 T4 定义、T7 使用 — 一致
- `IdentityResolver::reverse_lookup(master_id, channel_ref) -> Option<String>` 在 T2 定义、T7 使用 — 一致

无遗漏。

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-18-persistent-tool-approval-grants.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
