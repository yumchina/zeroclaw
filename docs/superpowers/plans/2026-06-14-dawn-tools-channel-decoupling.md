# dawn-tools 与 channel 解耦 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除 `zeroclaw-channels → dawn-tools` 的反向编译时依赖，让 dawn task 工具通过 0.8.0 既有的 `PerToolChannelHandle` 模式路由到任意支持的 channel。

**Architecture:** 在 `zeroclaw-api` 引入 `SendKind` 枚举（扩展 `SendMessage`）和 `ChannelOrigin` task-local；`DawnIMChannel::send` 内部 match kind 分支；dawn-tools 改用 channel handle 而非全局 mpsc bridge；删除桥接基础设施。

**Tech Stack:** Rust, tokio (task_local + mpsc), parking_lot::RwLock (现有), serde, anyhow

**Spec:** [`docs/superpowers/specs/2026-06-14-dawn-tools-channel-decoupling-design.md`](../specs/2026-06-14-dawn-tools-channel-decoupling-design.md)

---

## File Structure

按改动顺序列出，标注职责：

| 文件 | 职责 |
|------|------|
| `crates/zeroclaw-api/src/channel.rs` | **新增** `SendKind` 枚举、`SendMessage.kind` 字段、`SendMessage::ensure_text_kind` helper、`ChannelOrigin` struct、`CHANNEL_ORIGIN` task_local |
| `crates/zeroclaw-channels/src/dawn_im/channel.rs` | **修改** `Channel::send` impl 分发 kind；TaskSubmit/Query 路径内联 type=2000 CMD 编码；最后阶段删除 `send_status_message` |
| `crates/zeroclaw-config/src/dawn_task.rs` | **重写**：`DawnTasks`→`DawnTaskExecutors`、`tasks`→`executors`、`DawnTaskConfig`→`DawnTaskExecutorConfig`、加 `channel` 字段、`uid`→`recipient` |
| `crates/zeroclaw-config/src/schema.rs` | **修改** 3 处 Default 站点 + `Config.dawn_task` 类型 |
| `crates/zeroclaw-runtime/src/tools/mod.rs` | **修改** `AllToolsResult` 加 `task_channel_handle`；创建 handle 传给 tools；注册逻辑用新字段名 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | **修改** `register_channels_for_tools` 加 `task_channel_handle` 入参；填 channel map；`process_channel_message_body` 用 `ChannelOrigin`；**删除** bridge listener spawn + `CollectedChannels.dawn_im_channels` |
| `crates/zeroclaw-gateway/src/lib.rs` & `src/ws.rs` | **修改** `register_channels_for_tools` 调用：补 `task_channel_handle` 参数 |
| `crates/dawn-tools/src/task.rs` | **重写**：删除 bridge 全套，工具改用 `PerToolChannelHandle` 投递 `SendMessage{kind: SendKind::TaskSubmit{..}}` |
| `crates/dawn-tools/src/lib.rs` | **修改** re-export：去掉 `TaskMessage` / `TaskContext` / `TASK_CONTEXT` / `set_channel_bridge` |
| `crates/dawn-tools/Cargo.toml` | **删除** `parking_lot` dep |
| `crates/zeroclaw-channels/Cargo.toml` | **删除** `dawn-tools.workspace = true` dep（关键解耦） |
| `crates/zeroclaw-channels/src/cli.rs` | **修改** 2 处 `SendMessage { ... }` test 字面量补 `kind` 字段（或用 `..Default::default()`） |

---

## Task 1: 在 zeroclaw-api 新增 SendKind 枚举

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`（在 `SendMessage` struct 之前插入）

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-api/src/channel.rs` 文件末尾的测试模块（如无则新建一个 `#[cfg(test)] mod send_kind_tests`）追加：

```rust
#[cfg(test)]
mod send_kind_tests {
    use super::*;

    #[test]
    fn send_kind_default_is_text() {
        assert!(matches!(SendKind::default(), SendKind::Text));
    }

    #[test]
    fn send_kind_task_submit_holds_fields() {
        let kind = SendKind::TaskSubmit {
            task_type: 7,
            user_id: "u_alice".into(),
            user_text: "extract this pdf".into(),
            params: serde_json::json!({"files": []}),
        };
        match kind {
            SendKind::TaskSubmit { task_type, user_id, user_text, params } => {
                assert_eq!(task_type, 7);
                assert_eq!(user_id, "u_alice");
                assert_eq!(user_text, "extract this pdf");
                assert_eq!(params["files"], serde_json::json!([]));
            }
            _ => panic!("expected TaskSubmit"),
        }
    }

    #[test]
    fn send_kind_task_query_holds_fields() {
        let kind = SendKind::TaskQuery {
            task_type: 7,
            user_id: "u_alice".into(),
            task_id: "task_xyz".into(),
        };
        assert!(matches!(
            kind,
            SendKind::TaskQuery { task_type: 7, .. }
        ));
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-api send_kind`
Expected: FAIL with "cannot find type `SendKind`"

- [ ] **Step 3: 实现 SendKind 枚举**

在 `crates/zeroclaw-api/src/channel.rs` 的 `pub struct SendMessage {` 行之前插入：

```rust
/// 消息类型分类。决定 `Channel::send` 实现走哪条编码路径。
///
/// `Text` 是默认值，与 0.8.0 现有 30+ channel 行为一致。`TaskSubmit` /
/// `TaskQuery` 用于通过 channel 把任务投递给外部 executor — 仅由
/// `dawn_create_task` / `dawn_query_task` 工具构造，并由配置中
/// `[dawn_task.<n>].channel` 显式指定目标 channel。
#[derive(Debug, Clone, Default)]
pub enum SendKind {
    /// 普通用户对话消息。
    #[default]
    Text,
    /// 提交任务给 channel 对端的外部 executor。
    TaskSubmit {
        task_type: u8,
        user_id: String,
        user_text: String,
        params: serde_json::Value,
    },
    /// 查询任务状态。
    TaskQuery {
        task_type: u8,
        user_id: String,
        task_id: String,
    },
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-api send_kind`
Expected: PASS (3 个测试)

- [ ] **Step 5: 提交**

```bash
git add crates/zeroclaw-api/src/channel.rs
git commit -m "feat(api): add SendKind enum for typed message dispatch"
```

---

## Task 2: 在 zeroclaw-api 新增 ChannelOrigin task_local

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-api/src/channel.rs` 文件末尾追加：

```rust
#[cfg(test)]
mod channel_origin_tests {
    use super::*;

    #[test]
    fn channel_origin_default_is_empty() {
        let o = ChannelOrigin::default();
        assert!(o.from_uid.is_empty());
        assert!(o.channel_ref.is_empty());
        assert!(o.reply_target.is_empty());
    }

    #[tokio::test]
    async fn channel_origin_scope_round_trip() {
        let origin = ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.work".into(),
            reply_target: "1:u_alice".into(),
        };
        let read_back = CHANNEL_ORIGIN
            .scope(origin.clone(), async {
                CHANNEL_ORIGIN.try_with(|o| o.clone()).unwrap()
            })
            .await;
        assert_eq!(read_back.from_uid, "u_alice");
        assert_eq!(read_back.channel_ref, "dawnim.work");
        assert_eq!(read_back.reply_target, "1:u_alice");
    }

    #[tokio::test]
    async fn channel_origin_outside_scope_is_default() {
        let result = CHANNEL_ORIGIN.try_with(|o| o.clone()).unwrap_or_default();
        assert!(result.from_uid.is_empty());
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-api channel_origin`
Expected: FAIL with "cannot find type `ChannelOrigin`"

- [ ] **Step 3: 实现 ChannelOrigin + CHANNEL_ORIGIN**

在 `crates/zeroclaw-api/src/channel.rs` 文件 `SendKind` 定义之后插入：

```rust
/// 一个 agent turn 的来源上下文。在 orchestrator 处理入站消息时构造并
/// 通过 [`CHANNEL_ORIGIN`] task-local scope 注入；工具调用栈内
/// `try_with` 读取，知道当前 turn 由哪个用户从哪个 channel 实例触发。
#[derive(Clone, Default, Debug)]
pub struct ChannelOrigin {
    /// 原始用户 ID（任何 channel-specific 后缀如 `_la_<bot_uid>` 已剥离）
    pub from_uid: String,
    /// Composite channel ref `"<type>.<alias>"`，例如 `"dawnim.work"`
    pub channel_ref: String,
    /// 原始 `ChannelMessage.reply_target` 值
    pub reply_target: String,
}

tokio::task_local! {
    /// Per-turn channel origin。orchestrator 的 `process_channel_message_body`
    /// 内 `CHANNEL_ORIGIN.scope(...)` 注入；channel-aware 工具
    /// （如 `dawn_create_task`）通过 `try_with` 读取。
    pub static CHANNEL_ORIGIN: ChannelOrigin;
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-api channel_origin`
Expected: PASS (3 个测试)

- [ ] **Step 5: 提交**

```bash
git add crates/zeroclaw-api/src/channel.rs
git commit -m "feat(api): add ChannelOrigin task-local for per-turn origin context"
```

---

## Task 3: 给 SendMessage 加 kind 字段 + ensure_text_kind helper

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`（`SendMessage` struct + 三处构造函数）
- Modify: `crates/zeroclaw-channels/src/cli.rs`（2 处测试结构字面量）

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-api/src/channel.rs` 末尾追加：

```rust
#[cfg(test)]
mod send_message_kind_tests {
    use super::*;

    #[test]
    fn send_message_new_defaults_to_text_kind() {
        let m = SendMessage::new("hello", "user");
        assert!(matches!(m.kind, SendKind::Text));
    }

    #[test]
    fn send_message_default_is_text_kind() {
        let m = SendMessage::default();
        assert!(matches!(m.kind, SendKind::Text));
        assert!(m.content.is_empty());
        assert!(m.recipient.is_empty());
    }

    #[test]
    fn ensure_text_kind_accepts_text() {
        let m = SendMessage::new("hi", "user");
        assert!(m.ensure_text_kind("test_channel").is_ok());
    }

    #[test]
    fn ensure_text_kind_rejects_task_submit() {
        let mut m = SendMessage::new("", "executor_uid");
        m.kind = SendKind::TaskSubmit {
            task_type: 1,
            user_id: "u".into(),
            user_text: "x".into(),
            params: serde_json::Value::Null,
        };
        let err = m.ensure_text_kind("wechat.main").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("wechat.main"), "got: {msg}");
        assert!(msg.contains("does not support kind"), "got: {msg}");
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-api send_message_kind`
Expected: FAIL with "no field `kind`" / "no method `ensure_text_kind`"

- [ ] **Step 3: 给 SendMessage 加 kind 字段 + Default**

修改 `crates/zeroclaw-api/src/channel.rs` 中 `SendMessage` struct（约第 94-107 行）：

把：
```rust
#[derive(Debug, Clone)]
pub struct SendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    pub thread_ts: Option<String>,
    pub cancellation_token: Option<CancellationToken>,
    pub attachments: Vec<MediaAttachment>,
    pub in_reply_to: Option<String>,
}
```

改为：
```rust
#[derive(Debug, Clone, Default)]
pub struct SendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    pub thread_ts: Option<String>,
    pub cancellation_token: Option<CancellationToken>,
    pub attachments: Vec<MediaAttachment>,
    pub in_reply_to: Option<String>,
    /// 消息类型分类（默认 [`SendKind::Text`]）。task 类型由
    /// `dawn_create_task` / `dawn_query_task` 工具显式构造。
    pub kind: SendKind,
}
```

- [ ] **Step 4: 给 SendMessage::new 和 with_subject 补 kind 默认值**

修改 `crates/zeroclaw-api/src/channel.rs` 中 `SendMessage::new`（约第 111-121 行）：

把：
```rust
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
            cancellation_token: None,
            attachments: vec![],
            in_reply_to: None,
        }
    }
```

改为：
```rust
    pub fn new(content: impl Into<String>, recipient: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: None,
            thread_ts: None,
            cancellation_token: None,
            attachments: vec![],
            in_reply_to: None,
            kind: SendKind::Text,
        }
    }
```

把 `with_subject` 同样补充：

```rust
    pub fn with_subject(
        content: impl Into<String>,
        recipient: impl Into<String>,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            subject: Some(subject.into()),
            thread_ts: None,
            cancellation_token: None,
            attachments: vec![],
            in_reply_to: None,
            kind: SendKind::Text,
        }
    }
```

- [ ] **Step 5: 实现 ensure_text_kind helper**

在 `crates/zeroclaw-api/src/channel.rs` 中 `SendMessage` 的 builder impl 块（包含 `pub fn subject` / `pub fn in_thread` 等的 impl 块）内追加：

```rust
    /// 不支持非 Text kind 的 channel 可在自己 `Channel::send` impl 顶部
    /// 调用此 helper，看到非 Text kind 时返回带 channel name 的可读 Err。
    pub fn ensure_text_kind(&self, channel_name: &str) -> anyhow::Result<()> {
        if !matches!(self.kind, SendKind::Text) {
            anyhow::bail!(
                "channel '{}' does not support kind={:?}",
                channel_name,
                self.kind,
            );
        }
        Ok(())
    }
```

- [ ] **Step 6: 修复 cli.rs 测试中的 SendMessage struct 字面量**

修改 `crates/zeroclaw-channels/src/cli.rs:91-99` 和 `:108-116` 两处测试。

把：
```rust
            .send(&SendMessage {
                content: "hello".into(),
                recipient: "user".into(),
                subject: None,
                thread_ts: None,
                cancellation_token: None,
                attachments: vec![],
                in_reply_to: None,
            })
```

改为：
```rust
            .send(&SendMessage {
                content: "hello".into(),
                recipient: "user".into(),
                ..Default::default()
            })
```

对 `:108` 处同样处理（content 和 recipient 都是空串，可改为 `SendMessage::default()`）：

把：
```rust
            .send(&SendMessage {
                content: String::new(),
                recipient: String::new(),
                subject: None,
                thread_ts: None,
                cancellation_token: None,
                attachments: vec![],
                in_reply_to: None,
            })
```

改为：
```rust
            .send(&SendMessage::default())
```

- [ ] **Step 7: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-api send_message_kind`
Expected: PASS (4 个测试)

Run: `cargo check -p zeroclaw-channels`
Expected: 编译通过（cli.rs 修复生效）

- [ ] **Step 8: 检查其它 crate 是否有同样的 SendMessage struct 字面量需修**

Run: `grep -rn "SendMessage {" --include="*.rs" crates/ | grep -v "pub struct SendMessage" | grep -v "impl SendMessage"`

如出现新的字面量，按 Step 6 同样方式补 `..Default::default()` 修复。

- [ ] **Step 9: 全工作区编译验证**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过，无 error

- [ ] **Step 10: 提交**

```bash
git add crates/zeroclaw-api/src/channel.rs crates/zeroclaw-channels/src/cli.rs
git commit -m "feat(api): add SendMessage.kind + ensure_text_kind helper"
```

---

## Task 4: DawnIMChannel::send 分发 kind（内联 type=2000 CMD 编码）

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`（`Channel::send` impl）

**说明：** 此 task 只在 `Channel::send` 内部加 `match kind` 分支。Text 路径完全不变；TaskSubmit/TaskQuery 路径内联 type=2000 CMD 编码逻辑（与现有 `send_status_message` 一致但单独实现，不调用它，因为 send_status_message 收到的 payload 是预先构造好的 JSON，而这里要从 SendKind variant 自己构造）。`send_status_message` 方法暂时保留（仍被 bridge listener 使用），下一轮 cleanup 再删。

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 文件末尾 `#[cfg(test)] mod` 内（如不存在则新建一个 `mod send_kind_dispatch_tests`）追加：

```rust
#[cfg(test)]
mod send_kind_dispatch_tests {
    use super::*;
    use zeroclaw_api::channel::{SendKind, SendMessage};

    fn build_channel() -> DawnIMChannel {
        // 用最小 DawnIMConfig 构造一个 channel；不实际连接 WS。
        let cfg = crate::dawn_im::config::DawnIMConfig::default_with_uid("bot_uid_1");
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn zeroclaw_api::memory_traits::Memory> = Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", tmp.path(), "send_kind_test")
                .unwrap(),
        );
        DawnIMChannel::from_config(&cfg, "test", tmp.path(), memory)
    }

    /// 验证 send 收到 TaskSubmit kind 时，不会走 Text 路径 — 即不会调用
    /// `encode_text_payload` 把空 `content` 编进 markdown payload。我们通过
    /// 观察"WS 未连接"错误信息来确认进入了 send_rpc 路径（与 Text 一致）。
    #[tokio::test]
    async fn send_task_submit_reaches_send_rpc_layer() {
        let ch = build_channel();
        let msg = SendMessage {
            recipient: "1878_xuanji_agent".into(),
            kind: SendKind::TaskSubmit {
                task_type: 1,
                user_id: "u_alice".into(),
                user_text: "extract this pdf".into(),
                params: serde_json::json!({"files": []}),
            },
            ..Default::default()
        };
        let err = ch.send(&msg).await.unwrap_err();
        // 没连接 WS 时，send_rpc 应该 bail "WebSocket not connected"。
        // 这间接证明 TaskSubmit 走了 send_rpc 路径（不是 Text 编码错误）。
        let err_str = err.to_string();
        assert!(
            err_str.contains("not connected") || err_str.contains("RPC"),
            "expected WS-layer error, got: {err_str}"
        );
    }

    #[tokio::test]
    async fn send_task_query_reaches_send_rpc_layer() {
        let ch = build_channel();
        let msg = SendMessage {
            recipient: "1878_xuanji_agent".into(),
            kind: SendKind::TaskQuery {
                task_type: 1,
                user_id: "u_alice".into(),
                task_id: "task_abc".into(),
            },
            ..Default::default()
        };
        let err = ch.send(&msg).await.unwrap_err();
        let err_str = err.to_string();
        assert!(
            err_str.contains("not connected") || err_str.contains("RPC"),
            "expected WS-layer error, got: {err_str}"
        );
    }
}
```

注意：可能需要在 `DawnIMConfig` 加一个测试 helper `default_with_uid(uid: &str) -> Self`。先确认是否存在：

Run: `grep -n "default_with_uid\|impl Default for DawnIMConfig" crates/zeroclaw-config/src/schema.rs`

如不存在则在 `crates/zeroclaw-channels/src/dawn_im/config.rs`（或合适位置）加测试模块或 helper，或者把构造逻辑展开成完整 `DawnIMConfig { ... }` 字面量。

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM send_kind_dispatch`
Expected: FAIL — 当前 `Channel::send` 不识别 task kind，可能走错路径（编码出错或意外的 Text 路径）

- [ ] **Step 3: 找到 DawnIMChannel::send 当前实现位置**

Run: `grep -n "impl Channel for DawnIMChannel" crates/zeroclaw-channels/src/dawn_im/channel.rs`
Run: `grep -n "async fn send" crates/zeroclaw-channels/src/dawn_im/channel.rs`

记下 `async fn send(&self, message: &SendMessage)` 的起止行号。

- [ ] **Step 4: 在 send 顶部加 match kind 分发**

在 `DawnIMChannel::send` impl 顶部加入 match：

```rust
    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        match &message.kind {
            zeroclaw_api::channel::SendKind::Text => {
                // 现有 Text 发送逻辑（保持完全不变 — 把下面整段原 send 代码包进这个分支）
                /* original Text-sending code here */
            }
            zeroclaw_api::channel::SendKind::TaskSubmit {
                task_type, user_id, user_text, params,
            } => {
                let payload = serde_json::json!({
                    "type": 2000,
                    "cmd": "dawn.create_task",
                    "param": {
                        "type": task_type,
                        "user_id": user_id,
                        "user_text": user_text,
                        "params": params,
                        "reply_to": self.uid,
                    }
                });
                self.send_task_payload(&message.recipient, payload).await
            }
            zeroclaw_api::channel::SendKind::TaskQuery {
                task_type, user_id, task_id,
            } => {
                let payload = serde_json::json!({
                    "type": 2000,
                    "cmd": "dawn.query_task",
                    "param": {
                        "type": task_type,
                        "user_id": user_id,
                        "task_id": task_id,
                        "reply_to": self.uid,
                    }
                });
                self.send_task_payload(&message.recipient, payload).await
            }
        }
    }
```

- [ ] **Step 5: 实现 send_task_payload 私有 helper**

在 `DawnIMChannel` 的 `impl` 块（与 `send_status_message` 同一区域）追加：

```rust
    /// 把一条预先构造好的 type=2000 CMD payload 编码为 base64 → SendParams →
    /// 经 send_rpc 投递。被 `Channel::send` 的 TaskSubmit / TaskQuery 分支
    /// 复用，避免与 `send_status_message`（被旧 bridge listener 使用）
    /// 互相耦合。
    async fn send_task_payload(
        &self,
        recipient: &str,
        payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        let payload_bytes = serde_json::to_vec(&payload)?;
        let payload_b64 = base64::engine::general_purpose::STANDARD.encode(&payload_bytes);
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id: recipient.to_string(),
            channel_type: 1,
            payload: serde_json::Value::String(payload_b64),
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }
```

- [ ] **Step 6: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM send_kind_dispatch`
Expected: PASS (2 个测试)

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM dawn_im`
Expected: 所有现有 dawn_im 测试仍通过（Text 路径回归）

- [ ] **Step 7: 提交**

```bash
git add crates/zeroclaw-channels/src/dawn_im/channel.rs
git commit -m "feat(channels/dawn_im): dispatch Channel::send on SendKind"
```

---

## Task 5: 重命名 dawn_task 配置类型 + 加 channel/recipient 字段

**Files:**
- Rewrite: `crates/zeroclaw-config/src/dawn_task.rs`
- Modify: `crates/zeroclaw-config/src/schema.rs`（3 处 Default 站点 + Config 字段类型）
- Modify: `crates/dawn-tools/src/task.rs`（type imports + 字段访问 + 测试 TOML）
- Modify: `crates/zeroclaw-runtime/src/tools/mod.rs`（`.tasks` → `.executors`）

**说明：** 这是一次原子的跨 crate 重命名 + schema 扩展。dawn-tools 的 bridge 逻辑暂时保留（下一轮删），只更新它对 `DawnTaskConfig`/`DawnTasks` 的引用。

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-config/src/dawn_task.rs` 末尾的 `#[cfg(test)] mod tests` 内追加测试（如已有同名测试先备份名字）：

```rust
    #[test]
    fn parse_with_channel_and_recipient_fields() {
        let toml = r#"
            ["1"]
            channel = "dawnim.work"
            recipient = "1878_xuanji_agent"
            name = "璇玑文档提取"
            description = "extract docs"
        "#;
        let cfg: DawnTaskExecutors = toml::from_str(toml).unwrap();
        let exec = cfg.get_by_type(1).expect("type 1 present");
        assert_eq!(exec.channel, "dawnim.work");
        assert_eq!(exec.recipient, "1878_xuanji_agent");
        assert_eq!(exec.name, "璇玑文档提取");
    }

    #[test]
    fn default_executors_collection_is_empty() {
        let cfg = DawnTaskExecutors::default();
        assert!(cfg.executors.is_empty());
        assert!(cfg.get_by_type(1).is_none());
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-config dawn_task::tests::parse_with_channel_and_recipient_fields`
Expected: FAIL with "cannot find type `DawnTaskExecutors`"

- [ ] **Step 3: 重写 dawn_task.rs**

完全替换 `crates/zeroclaw-config/src/dawn_task.rs` 内容为：

```rust
//! Dawn task type configuration.
//!
//! Maps task type IDs (1=doc extraction, 2=code analysis, ...) to the
//! channel + addressee that handles each type. Used by the
//! `dawn_create_task` / `dawn_query_task` tools to route a caller-supplied
//! task type to the right executor on the Dawn platform.
//!
//! Example TOML:
//!
//! ```toml
//! [dawn_task.1]
//! channel     = "dawnim.work"
//! recipient   = "1878_xuanji_agent"
//! name        = "璇玑文档提取"
//! description = "extract PDF/Word/PPT/Excel content"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::traits::{HasPropKind, PropKind};

/// 单个 task 类型的 executor 配置（"任务由谁/通过哪个 channel 执行"）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnTaskExecutorConfig {
    /// Composite channel key `"<type>.<alias>"`，例如 `"dawnim.work"`。
    /// 该 channel 必须由 `[channels.<type>.<alias>]` 配置启用，且其
    /// `Channel::send` impl 支持 SendKind::TaskSubmit / TaskQuery。
    pub channel: String,
    /// Channel-specific 寻址：
    /// - dawnim: agent UID, e.g. `"1878_xuanji_agent"`
    /// - wechat: openid / group_id
    /// - slack: webhook URL or user/channel ID
    pub recipient: String,
    /// 人类可读名称（日志 + 运维 UX）
    pub name: String,
    /// 任务描述（注入到 dawn_create_task 工具的 description）
    pub description: String,
}

/// task type id → executor 配置的注册表。
///
/// TOML 表 key 永远是字符串，所以数字 task type 在查找时再 to_string()。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnTaskExecutors {
    #[serde(flatten)]
    pub executors: HashMap<String, DawnTaskExecutorConfig>,
}

impl HasPropKind for DawnTaskExecutors {
    const PROP_KIND: PropKind = PropKind::Object;
}

impl DawnTaskExecutors {
    /// 按 task type id 查找 executor 配置。
    pub fn get_by_type(&self, task_type: u8) -> Option<&DawnTaskExecutorConfig> {
        self.executors.get(&task_type.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_channel_and_recipient_fields() {
        let toml = r#"
            ["1"]
            channel = "dawnim.work"
            recipient = "1878_xuanji_agent"
            name = "璇玑文档提取"
            description = "extract docs"
        "#;
        let cfg: DawnTaskExecutors = toml::from_str(toml).unwrap();
        let exec = cfg.get_by_type(1).expect("type 1 present");
        assert_eq!(exec.channel, "dawnim.work");
        assert_eq!(exec.recipient, "1878_xuanji_agent");
        assert_eq!(exec.name, "璇玑文档提取");
    }

    #[test]
    fn default_executors_collection_is_empty() {
        let cfg = DawnTaskExecutors::default();
        assert!(cfg.executors.is_empty());
        assert!(cfg.get_by_type(1).is_none());
    }

    #[test]
    fn get_by_unknown_type_returns_none() {
        let cfg = DawnTaskExecutors::default();
        assert!(cfg.get_by_type(99).is_none());
    }

    #[test]
    fn missing_channel_field_fails_to_parse() {
        let toml = r#"
            ["1"]
            recipient = "x"
            name = "n"
            description = "d"
        "#;
        let err = toml::from_str::<DawnTaskExecutors>(toml).unwrap_err();
        assert!(err.to_string().contains("channel"), "got: {err}");
    }
}
```

- [ ] **Step 4: 修改 schema.rs 的 Config 字段类型 + 3 处 Default 站点**

修改 `crates/zeroclaw-config/src/schema.rs`：

把 `Config` struct 中 `dawn_task` 字段的类型：

```rust
    pub dawn_task: crate::dawn_task::DawnTasks,
```

改为：

```rust
    pub dawn_task: crate::dawn_task::DawnTaskExecutors,
```

把所有 3 处 Default 初始化：

```rust
dawn_task: crate::dawn_task::DawnTasks::default(),
```

改为：

```rust
dawn_task: crate::dawn_task::DawnTaskExecutors::default(),
```

Run: `grep -n "DawnTasks" crates/zeroclaw-config/src/schema.rs`
Expected: 无输出（全部替换完毕）

- [ ] **Step 5: 修改 dawn-tools/src/task.rs 的类型 import 和字段访问**

修改 `crates/dawn-tools/src/task.rs`：

把 import：
```rust
use zeroclaw_config::dawn_task::{DawnTaskConfig, DawnTasks};
```

改为：
```rust
use zeroclaw_config::dawn_task::DawnTaskExecutorConfig;
```

把 helper 函数 `resolve_task`（或当前名 `resolve_executor`）的返回类型：
```rust
fn resolve_task(config: &Arc<Config>, task_type: u8) -> Option<DawnTaskConfig> {
```

改为：
```rust
fn resolve_executor(config: &Arc<Config>, task_type: u8) -> Option<DawnTaskExecutorConfig> {
```

并且函数体：
```rust
    config.dawn_task.get_by_type(task_type).cloned()
```

保持不变（API 签名不变）。

更新调用点：把所有 `resolve_task(...)` 调用改为 `resolve_executor(...)`，把所有 `task.uid` 字段访问（如果存在）改为 `executor.recipient`。

注意：此 task 不修改 `CreateTaskTool` / `QueryTaskTool` 的执行逻辑 — 它们仍走旧 bridge 路径，只是类型名变了。bridge 路径完整功能不破坏。

修改测试 TOML：在测试模块的 `make_config_with_dawnim` 或同类 helper 中，把：
```toml
[dawn_task.1]
uid = "1878_xuanji_agent"
name = "璇玑"
description = "doc extraction"
```

改为：
```toml
[dawn_task.1]
channel = "dawnim.work"
recipient = "1878_xuanji_agent"
name = "璇玑"
description = "doc extraction"
```

并在工具 execute 中如有 `executor.uid` 访问，改为 `executor.recipient`。

- [ ] **Step 6: 修改 zeroclaw-runtime/src/tools/mod.rs 的字段访问**

找到 dawn task 工具注册块（grep `dawn_task.tasks` 或 `is_empty`）：

把：
```rust
if !root_config.dawn_task.tasks.is_empty() {
```
和：
```rust
"task_types": root_config.dawn_task.tasks.keys().collect::<Vec<_>>(),
```

改为：
```rust
if !root_config.dawn_task.executors.is_empty() {
```
和：
```rust
"task_types": root_config.dawn_task.executors.keys().collect::<Vec<_>>(),
```

- [ ] **Step 7: 全工作区编译验证**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过

- [ ] **Step 8: 运行相关测试**

Run: `cargo test -p zeroclaw-config dawn_task`
Expected: PASS (4 个测试)

Run: `cargo test -p dawn-tools task`
Expected: PASS (bridge 路径测试仍通过)

- [ ] **Step 9: 提交**

```bash
git add crates/zeroclaw-config/src/dawn_task.rs \
        crates/zeroclaw-config/src/schema.rs \
        crates/dawn-tools/src/task.rs \
        crates/zeroclaw-runtime/src/tools/mod.rs
git commit -m "refactor(config): rename DawnTasks->DawnTaskExecutors, add channel/recipient fields"
```

---

## Task 6: zeroclaw-runtime 创建 task_channel_handle 并传给工具

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/mod.rs`（`AllToolsResult` + `all_tools_with_runtime`）
- Modify: `crates/dawn-tools/src/task.rs`（`CreateTaskTool::new` / `QueryTaskTool::new` 增加 channel handle 参数 — 暂时不使用，下一 task 才接线）

**说明：** 此 task 引入 handle 参数 + 创建空 handle。tools 接收 handle 但暂不使用（仍走 bridge）。下一 task（Task 8）才让 tools 真正用 handle。本 task 让运行时层把"接收 handle"的能力先具备。

- [ ] **Step 1: 给 CreateTaskTool 和 QueryTaskTool 加 channels 字段**

修改 `crates/dawn-tools/src/task.rs`：

把：
```rust
pub struct CreateTaskTool {
    config: Arc<Config>,
}

impl CreateTaskTool {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }
}
```

改为：
```rust
pub struct CreateTaskTool {
    config: Arc<Config>,
    /// Late-bound channel registry; populated by
    /// orchestrator::register_channels_for_tools at startup.
    #[allow(dead_code)] // 下一个 task 才开始使用
    channels: zeroclaw_api::channel::PerToolChannelHandle,
}

impl CreateTaskTool {
    pub fn new(
        config: Arc<Config>,
        channels: zeroclaw_api::channel::PerToolChannelHandle,
    ) -> Self {
        Self { config, channels }
    }
}
```

QueryTaskTool 同样处理（字段、构造函数完全平行）。

**注意：** `PerToolChannelHandle` 当前定义在 `zeroclaw-runtime/src/tools/mod.rs`。但 dawn-tools 不依赖 zeroclaw-runtime。需要 **把 type alias 提到 zeroclaw-api**：

- [ ] **Step 2: 把 PerToolChannelHandle 类型别名提到 zeroclaw-api**

修改 `crates/zeroclaw-api/src/channel.rs`，在 `Channel` trait 定义之后追加：

```rust
/// 共享 channel registry handle — `Arc<RwLock<HashMap<channel_key, Channel>>>`。
///
/// 工具拿到这个 handle 在自己 execute 时按 channel key 找 Arc<dyn Channel>。
/// orchestrator 启动后填充。Key 形式：`"<type>.<alias>"`（如 `"dawnim.work"`）。
pub type PerToolChannelHandle =
    std::sync::Arc<parking_lot::RwLock<std::collections::HashMap<String, std::sync::Arc<dyn Channel>>>>;
```

确认 `zeroclaw-api` Cargo.toml 已含 `parking_lot` 依赖；如未含则添加。

Run: `grep -n "parking_lot" crates/zeroclaw-api/Cargo.toml`
如无输出，在 `[dependencies]` 块加：
```toml
parking_lot = "0.12"
```

- [ ] **Step 3: 修改 zeroclaw-runtime 用 api 中的 PerToolChannelHandle 别名**

修改 `crates/zeroclaw-runtime/src/tools/mod.rs`：

把原本定义在那里的 `pub type PerToolChannelHandle = ...;` 改为 re-export：

```rust
pub use zeroclaw_api::channel::PerToolChannelHandle;
```

（删除原 type alias 行）

- [ ] **Step 4: 编译检验 dawn-tools 现在能引用**

Run: `cargo check -p dawn-tools`
Expected: 编译通过

- [ ] **Step 5: 在 AllToolsResult 加 task_channel_handle 字段**

修改 `crates/zeroclaw-runtime/src/tools/mod.rs` 中 `AllToolsResult` struct（约第 422 行起）：

在现有 handle 字段（`escalate_handle`）之后追加：

```rust
    /// Channel registry handle shared with dawn task tools.
    pub task_channel_handle: PerToolChannelHandle,
```

- [ ] **Step 6: all_tools_with_runtime 创建 handle 并传给 tools**

修改 `crates/zeroclaw-runtime/src/tools/mod.rs` 中 `all_tools_with_runtime` 函数。

找到 dawn task 工具注册块（grep `CreateTaskTool::new`），改：

把：
```rust
        let cfg_arc = Arc::new(root_config.clone());
        tool_arcs.push(Arc::new(CreateTaskTool::new(cfg_arc.clone())));
        tool_arcs.push(Arc::new(QueryTaskTool::new(cfg_arc)));
```

改为：
```rust
        let cfg_arc = Arc::new(root_config.clone());
        tool_arcs.push(Arc::new(CreateTaskTool::new(
            cfg_arc.clone(),
            task_channel_handle.clone(),
        )));
        tool_arcs.push(Arc::new(QueryTaskTool::new(
            cfg_arc,
            task_channel_handle.clone(),
        )));
```

在该 dawn task block 之前（与其他 handle 创建处一致的位置）创建 handle：

```rust
    let task_channel_handle: PerToolChannelHandle = Arc::new(RwLock::new(HashMap::new()));
```

并在函数末尾返回 `AllToolsResult` 时把它加进去（找到现有 return 语句，加 `task_channel_handle,` 字段）。

- [ ] **Step 7: 全工作区编译验证**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过（可能有"unused variable: task_channel_handle"或 dead_code warning — 接受，下一 task 接线后消失）

- [ ] **Step 8: 提交**

```bash
git add crates/zeroclaw-api/src/channel.rs \
        crates/zeroclaw-api/Cargo.toml \
        crates/zeroclaw-runtime/src/tools/mod.rs \
        crates/dawn-tools/src/task.rs
git commit -m "refactor(runtime): pipe task_channel_handle through tools factory"
```

---

## Task 7: 给 register_channels_for_tools 加 task_channel_handle 参数

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（`register_channels_for_tools` 函数）
- Modify: `crates/zeroclaw-gateway/src/lib.rs`（1 处调用点）
- Modify: `crates/zeroclaw-gateway/src/ws.rs`（1 处调用点）

- [ ] **Step 1: 修改 register_channels_for_tools 签名和实现**

修改 `crates/zeroclaw-channels/src/orchestrator/mod.rs` 中 `register_channels_for_tools`（约第 6678 行）。

签名加入参数（与其他 handle 入参排列对齐）：

```rust
pub fn register_channels_for_tools(
    config: &Config,
    ask_user_handle: &Option<tools::PerToolChannelHandle>,
    reaction_handle: &Option<tools::PerToolChannelHandle>,
    poll_handle: &Option<tools::PerToolChannelHandle>,
    escalate_handle: &Option<tools::PerToolChannelHandle>,
    task_channel_handle: &Option<tools::PerToolChannelHandle>,  // ← 新增
) -> Vec<String> {
```

在 handles 数组中加入新 handle：

把：
```rust
    let handles = [
        ask_user_handle.as_ref(),
        reaction_handle.as_ref(),
        poll_handle.as_ref(),
        escalate_handle.as_ref(),
    ];
```

改为：
```rust
    let handles = [
        ask_user_handle.as_ref(),
        reaction_handle.as_ref(),
        poll_handle.as_ref(),
        escalate_handle.as_ref(),
        task_channel_handle.as_ref(),
    ];
```

- [ ] **Step 2: 更新 gateway lib.rs 调用点**

修改 `crates/zeroclaw-gateway/src/lib.rs:826` 的 `register_channels_for_tools(...)` 调用，在最后一个 handle 之后补：

```rust
    &all_tools_result.task_channel_handle.clone().map(Some).unwrap_or(None).into(),
```

更准确地，先把 handle 包成 Some：

```rust
    let task_channel_handle = Some(all_tools_result.task_channel_handle.clone());
```

然后在 register_channels_for_tools 调用里：

```rust
let channel_names = zeroclaw_channels::orchestrator::register_channels_for_tools(
    &config,
    &all_tools_result.ask_user_handle,
    &Some(all_tools_result.reaction_handle.clone()),
    &all_tools_result.poll_handle,
    &all_tools_result.escalate_handle,
    &task_channel_handle,
);
```

具体匹配现有代码风格 — 找到该处看完整调用形式再改。

- [ ] **Step 3: 更新 gateway ws.rs 调用点**

修改 `crates/zeroclaw-gateway/src/ws.rs:482` 同样模式 — 补上 `&task_channel_handle` 参数。

- [ ] **Step 4: 检查是否有更多调用点**

Run: `grep -rn "register_channels_for_tools" --include="*.rs" crates/`
Expected: 全部 3-4 个调用点都已更新

- [ ] **Step 5: 全工作区编译验证**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs \
        crates/zeroclaw-gateway/src/lib.rs \
        crates/zeroclaw-gateway/src/ws.rs
git commit -m "refactor(orchestrator): plumb task_channel_handle through register_channels_for_tools"
```

---

## Task 8: 重写 CreateTaskTool / QueryTaskTool 使用 Channel handle + 替换 TaskContext 为 ChannelOrigin

**Files:**
- Modify: `crates/dawn-tools/src/task.rs`（工具 execute 实现 + 测试）
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`（`process_channel_message_body` 中 TaskContext 替换为 ChannelOrigin）

**说明：** 此 task 切换工具行为 — 不再 push TaskMessage 到 bridge，改为通过 handle 查 channel 并调 `channel.send(SendMessage{kind: SendKind::TaskSubmit{..}})`. 旧 bridge 符号（`TaskMessage`、`CHANNEL_BRIDGE`、`set_channel_bridge`、`TaskContext`、`TASK_CONTEXT`）保留在 dawn-tools（仍 export），下一 task 才删 — 这样 orchestrator 的 bridge listener spawn 不至于在本 task 后立刻 broken。

- [ ] **Step 1: 写新测试**

替换 `crates/dawn-tools/src/task.rs` 测试模块中的 `create_and_query_push_payloads_via_bridge` 测试为基于 channel handle 的版本：

在测试模块中先实现 mock channel：

```rust
    use std::sync::Mutex as StdMutex;
    use zeroclaw_api::channel::{Channel, ChannelMessage, PerToolChannelHandle, SendMessage};

    /// Records every SendMessage passed to its `send()`.
    struct RecordingChannel {
        name: &'static str,
        recorded: StdMutex<Vec<SendMessage>>,
    }

    impl RecordingChannel {
        fn new(name: &'static str) -> Self {
            Self { name, recorded: StdMutex::new(Vec::new()) }
        }
        fn take(&self) -> Vec<SendMessage> {
            std::mem::take(&mut *self.recorded.lock().unwrap())
        }
    }

    impl zeroclaw_api::attribution::Attributable for RecordingChannel {
        fn attribution_role(&self) -> zeroclaw_api::attribution::Role {
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::Cli,
            )
        }
        fn attribution_alias(&self) -> Option<&str> { None }
    }

    #[async_trait::async_trait]
    impl Channel for RecordingChannel {
        fn name(&self) -> &str { self.name }
        async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
            self.recorded.lock().unwrap().push(message.clone());
            Ok(())
        }
        async fn listen(
            &self,
            _: tokio::sync::mpsc::Sender<ChannelMessage>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn make_handle_with_channel(
        alias: &str,
        ch: Arc<RecordingChannel>,
    ) -> PerToolChannelHandle {
        let map = std::collections::HashMap::from([(alias.to_string(), ch as Arc<dyn Channel>)]);
        Arc::new(parking_lot::RwLock::new(map))
    }
```

然后追加测试：

```rust
    #[tokio::test]
    async fn create_task_sends_via_channel_handle() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let ch = Arc::new(RecordingChannel::new("dawnim"));
        let handle = make_handle_with_channel("dawnim.work", ch.clone());

        let tool = CreateTaskTool::new(cfg, handle);
        let origin = zeroclaw_api::channel::ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.work".into(),
            reply_target: "1:u_alice".into(),
        };
        let result = zeroclaw_api::channel::CHANNEL_ORIGIN
            .scope(origin, async {
                tool.execute(serde_json::json!({
                    "type": 1,
                    "user_text": "extract this pdf",
                    "params": {"files": []}
                }))
                .await
            })
            .await
            .unwrap();
        assert!(result.success);

        let recorded = ch.take();
        assert_eq!(recorded.len(), 1);
        let msg = &recorded[0];
        assert_eq!(msg.recipient, "1878_xuanji_agent");
        match &msg.kind {
            zeroclaw_api::channel::SendKind::TaskSubmit {
                task_type, user_id, user_text, params,
            } => {
                assert_eq!(*task_type, 1);
                assert_eq!(user_id, "u_alice");
                assert_eq!(user_text, "extract this pdf");
                assert_eq!(params["files"], serde_json::json!([]));
            }
            other => panic!("expected TaskSubmit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_task_errors_when_channel_not_in_handle() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        // 注册一个错的 alias，让 handle 查 dawnim.work 时 miss
        let ch = Arc::new(RecordingChannel::new("dawnim"));
        let handle = make_handle_with_channel("dawnim.other", ch);

        let tool = CreateTaskTool::new(cfg, handle);
        let origin = zeroclaw_api::channel::ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.other".into(),
            reply_target: "1:u_alice".into(),
        };
        let err = zeroclaw_api::channel::CHANNEL_ORIGIN
            .scope(origin, async {
                tool.execute(serde_json::json!({"type": 1, "user_text": "x", "params": {}}))
                    .await
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("dawnim.work"), "got: {err}");
    }

    #[tokio::test]
    async fn query_task_sends_via_channel_handle() {
        let cfg = make_config_with_dawnim("work", "bot_uid_1");
        let ch = Arc::new(RecordingChannel::new("dawnim"));
        let handle = make_handle_with_channel("dawnim.work", ch.clone());

        let tool = QueryTaskTool::new(cfg, handle);
        let origin = zeroclaw_api::channel::ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.work".into(),
            reply_target: "1:u_alice".into(),
        };
        let result = zeroclaw_api::channel::CHANNEL_ORIGIN
            .scope(origin, async {
                tool.execute(serde_json::json!({"type": 1, "task_id": "task_abc"}))
                    .await
            })
            .await
            .unwrap();
        assert!(result.success);

        let recorded = ch.take();
        assert_eq!(recorded.len(), 1);
        match &recorded[0].kind {
            zeroclaw_api::channel::SendKind::TaskQuery {
                task_type, user_id, task_id,
            } => {
                assert_eq!(*task_type, 1);
                assert_eq!(user_id, "u_alice");
                assert_eq!(task_id, "task_abc");
            }
            other => panic!("expected TaskQuery, got {other:?}"),
        }
    }
```

并删除旧的 `create_and_query_push_payloads_via_bridge` 测试（基于 bridge 的版本）—— 它依赖即将被删除的 `TaskMessage` 等符号。

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p dawn-tools task::tests::create_task_sends_via_channel_handle`
Expected: FAIL — 当前 CreateTaskTool::execute 还在走 bridge

- [ ] **Step 3: 重写 CreateTaskTool::execute**

修改 `crates/dawn-tools/src/task.rs` 中 `CreateTaskTool` 的 `execute` 实现：

把现有 execute（走 bridge_sender）整个替换为：

```rust
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_type = args
            .get("type")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("缺少 type 参数"))? as u8;

        let executor = resolve_executor(&self.config, task_type)
            .ok_or_else(|| anyhow::anyhow!("未配置 type={} 的 dawn task", task_type))?;

        let origin = zeroclaw_api::channel::CHANNEL_ORIGIN
            .try_with(|o| o.clone())
            .unwrap_or_default();

        let channel: std::sync::Arc<dyn zeroclaw_api::channel::Channel> = {
            let map = self.channels.read();
            map.get(&executor.channel).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "channel '{}' 未注册或未启用（dawn_task type={} 配置依赖此 channel）",
                    executor.channel,
                    task_type,
                )
            })?
        };

        let user_text = args
            .get("user_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let params = args.get("params").cloned().unwrap_or(serde_json::Value::Null);

        let msg = zeroclaw_api::channel::SendMessage {
            recipient: executor.recipient.clone(),
            kind: zeroclaw_api::channel::SendKind::TaskSubmit {
                task_type,
                user_id: origin.from_uid,
                user_text,
                params,
            },
            ..Default::default()
        };

        channel.send(&msg).await.map_err(|e| {
            anyhow::anyhow!("通过 channel '{}' 投递任务失败：{e}", executor.channel)
        })?;

        Ok(ToolResult {
            success: true,
            output: format!("已提交任务到 {}，等待处理，完成后会主动通知您", executor.name),
            error: None,
        })
    }
```

- [ ] **Step 4: 重写 QueryTaskTool::execute**

把 `QueryTaskTool` 的 `execute` 实现替换为：

```rust
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let task_type = args
            .get("type")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("缺少 type 参数"))? as u8;
        let task_id = args
            .get("task_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("缺少 task_id 参数"))?
            .to_string();

        let executor = resolve_executor(&self.config, task_type)
            .ok_or_else(|| anyhow::anyhow!("未配置 type={} 的 dawn task", task_type))?;

        let origin = zeroclaw_api::channel::CHANNEL_ORIGIN
            .try_with(|o| o.clone())
            .unwrap_or_default();

        let channel: std::sync::Arc<dyn zeroclaw_api::channel::Channel> = {
            let map = self.channels.read();
            map.get(&executor.channel).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "channel '{}' 未注册或未启用（dawn_task type={} 配置依赖此 channel）",
                    executor.channel,
                    task_type,
                )
            })?
        };

        let msg = zeroclaw_api::channel::SendMessage {
            recipient: executor.recipient.clone(),
            kind: zeroclaw_api::channel::SendKind::TaskQuery {
                task_type,
                user_id: origin.from_uid,
                task_id: task_id.clone(),
            },
            ..Default::default()
        };

        channel.send(&msg).await.map_err(|e| {
            anyhow::anyhow!("通过 channel '{}' 投递查询失败：{e}", executor.channel)
        })?;

        Ok(ToolResult {
            success: true,
            output: format!("已发送查询请求，task_id: {task_id}"),
            error: None,
        })
    }
```

- [ ] **Step 5: 删除 task.rs 中的 #[allow(dead_code)] 标记**

由于 `channels` 字段现在被实际使用了，删除 Task 6 加的 `#[allow(dead_code)]` 注解。

- [ ] **Step 6: 用 ChannelOrigin 替换 orchestrator 中的 TaskContext**

修改 `crates/zeroclaw-channels/src/orchestrator/mod.rs` 中 `process_channel_message_body`（约第 4654 行起 — 之前 Task 完成后的位置）。

把：
```rust
    let task_ctx = dawn_tools::TaskContext {
        from_uid: msg
            .sender
            .split("_la_")
            .next()
            .unwrap_or(msg.sender.as_str())
            .to_string(),
        reply_target: msg.reply_target.clone(),
        channel_alias: msg.channel_alias.clone().unwrap_or_default(),
    };
```

改为：
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
    };
```

把 scope 调用：
```rust
                        dawn_tools::TASK_CONTEXT.scope(
                            task_ctx.clone(),
```

改为：
```rust
                        zeroclaw_api::channel::CHANNEL_ORIGIN.scope(
                            channel_origin.clone(),
```

- [ ] **Step 7: 运行 dawn-tools 测试**

Run: `cargo test -p dawn-tools task`
Expected: PASS — 3 个新测试通过（create handles，create channel-not-found，query handles）

- [ ] **Step 8: 全工作区编译验证**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过

- [ ] **Step 9: 提交**

```bash
git add crates/dawn-tools/src/task.rs \
        crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(dawn-tools): route via PerToolChannelHandle instead of mpsc bridge"
```

---

## Task 9: 删除 orchestrator 中的 bridge listener + CollectedChannels.dawn_im_channels

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1: 删除 start_channels 中的 bridge listener spawn**

找到 `crates/zeroclaw-channels/src/orchestrator/mod.rs` 中包含 `// ── Tool → DawnIM bridge` 注释的区块（约第 9000-9090 行）。

整个删除该区块 — 从注释开始一直到最后一个 `});`（包含 `tokio::spawn(async move { ... bridge_rx.recv().await ... })` 的整个块）。

- [ ] **Step 2: 删除 CollectedChannels.dawn_im_channels 字段**

修改 `CollectedChannels` struct（约第 6720 行）：

把：
```rust
#[cfg_attr(not(feature = "channel-dawnIM"), allow(dead_code))]
#[derive(Default)]
struct CollectedChannels {
    channels: Vec<ConfiguredChannel>,
    #[cfg(feature = "channel-dawnIM")]
    dawn_im_channels: HashMap<String, Arc<crate::dawn_im::DawnIMChannel>>,
}
```

改为：
```rust
#[derive(Default)]
struct CollectedChannels {
    channels: Vec<ConfiguredChannel>,
}
```

- [ ] **Step 3: 删除 collect_configured_channels 中的 dawn_im_channels 填充逻辑**

找到 dawnim block 中的：

```rust
        let dawn_arc = Arc::new(DawnIMChannel::from_config(
            wk,
            alias.clone(),
            &config.data_dir,
            memory,
        ));
        dawn_im_channels.insert(alias.clone(), dawn_arc.clone());
        channels.push(ConfiguredChannel {
            display_name: "DawnIM",
            alias: Some(alias.clone()),
            channel: dawn_arc,
        });
```

改回直接构造（恢复 Task 完成前的形态）：

```rust
        channels.push(ConfiguredChannel {
            display_name: "DawnIM",
            alias: Some(alias.clone()),
            channel: Arc::new(DawnIMChannel::from_config(
                wk,
                alias.clone(),
                &config.data_dir,
                memory,
            )),
        });
```

并删除函数顶部的 `let mut dawn_im_channels: HashMap<...> = HashMap::new();` 声明。

- [ ] **Step 4: 删除 collect_configured_channels 返回值的 dawn_im_channels 字段**

函数尾部的：

```rust
    CollectedChannels {
        channels,
        #[cfg(feature = "channel-dawnIM")]
        dawn_im_channels,
    }
```

改为：
```rust
    CollectedChannels { channels }
```

- [ ] **Step 5: 删除 start_channels 中对 dawn_im_channels_for_bridge 的解构**

找到 `let CollectedChannels { channels: mut configured_channels, ... dawn_im_channels: dawn_im_channels_for_bridge, } = collect_configured_channels(...)`：

改为：
```rust
            #[allow(unused_mut)]
            let CollectedChannels {
                channels: mut configured_channels,
            } = collect_configured_channels(&config_arc, "runtime startup", &tool_specs);
```

- [ ] **Step 6: 全工作区编译验证**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过（应有 unused import 警告关于 `HashMap` 或类似 — 接受或删除）

如有 unused import warning，根据提示删除。

- [ ] **Step 7: 运行测试**

Run: `cargo test -p zeroclaw-channels --lib --features channel-dawnIM dawn_im`
Expected: PASS（dawn_im 测试不受影响）

- [ ] **Step 8: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "chore(orchestrator): remove bridge listener (replaced by PerToolChannelHandle)"
```

---

## Task 10: 删除 dawn-tools 中的 bridge 全套 + DawnIMChannel::send_status_message + Cargo.toml 清理

**Files:**
- Modify: `crates/dawn-tools/src/task.rs`（删除 TaskMessage / CHANNEL_BRIDGE / set_channel_bridge / bridge_sender / TaskContext / TASK_CONTEXT）
- Modify: `crates/dawn-tools/src/lib.rs`（删除 re-exports）
- Modify: `crates/dawn-tools/Cargo.toml`（删除 parking_lot 依赖）
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`（删除 send_status_message）
- Modify: `crates/zeroclaw-channels/Cargo.toml`（删除 dawn-tools 依赖 — 关键解耦）

- [ ] **Step 1: 删除 dawn-tools/src/task.rs 中的 bridge 全套**

从 `crates/dawn-tools/src/task.rs` 中删除以下定义：
- `pub struct TaskMessage { ... }` 及其 `#[derive]`
- `static CHANNEL_BRIDGE: RwLock<...>` 静态
- `pub fn set_channel_bridge(...)` 函数
- `fn bridge_sender() -> ...` 函数
- `pub struct TaskContext { ... }`
- `tokio::task_local! { pub static TASK_CONTEXT: TaskContext; }`
- `fn read_context() -> TaskContext { ... }` 函数（如已无人使用）

删除 `use parking_lot::RwLock;` 导入（若不再使用）。

- [ ] **Step 2: 更新 dawn-tools/src/lib.rs re-export**

修改 `crates/dawn-tools/src/lib.rs`，把：

```rust
pub use task::{
    CreateTaskTool, QueryTaskTool, TASK_CONTEXT, TaskContext, TaskMessage, set_channel_bridge,
};
```

改为：
```rust
pub use task::{CreateTaskTool, QueryTaskTool};
```

更新文件顶部的 module-level 文档字符串，去掉对 bridge 的描述（如有）。

- [ ] **Step 3: 删除 DawnIMChannel::send_status_message**

修改 `crates/zeroclaw-channels/src/dawn_im/channel.rs`，找到并删除整个 `pub async fn send_status_message(...)` 方法（约第 856 行起）。

确认 grep 无残留引用：
Run: `grep -rn "send_status_message" --include="*.rs" crates/`
Expected: 仅留注释中的可能历史性引用（如有，一并删除）

- [ ] **Step 4: 删除 dawn-tools/Cargo.toml 中的 parking_lot**

修改 `crates/dawn-tools/Cargo.toml`，删除 `parking_lot = "0.12"` 行。

- [ ] **Step 5: 删除 zeroclaw-channels/Cargo.toml 中的 dawn-tools 依赖**

修改 `crates/zeroclaw-channels/Cargo.toml`，删除 `dawn-tools.workspace = true` 行。

**这是核心解耦点** — 完成后 `zeroclaw-channels` 不再编译时依赖 `dawn-tools`。

- [ ] **Step 6: 全工作区编译验证**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过

如出现"unresolved import dawn_tools"错误，说明仍有遗漏的引用 — 按错误信息修复。

- [ ] **Step 7: 运行测试**

Run: `cargo test -p dawn-tools`
Expected: PASS

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM dawn_im`
Expected: PASS

- [ ] **Step 8: 提交**

```bash
git add crates/dawn-tools/src/task.rs \
        crates/dawn-tools/src/lib.rs \
        crates/dawn-tools/Cargo.toml \
        crates/zeroclaw-channels/src/dawn_im/channel.rs \
        crates/zeroclaw-channels/Cargo.toml
git commit -m "chore: drop dawn-tools->channels bridge; remove zeroclaw-channels->dawn-tools dep"
```

---

## Task 11: 加 validate_dawn_task_executors 软校验

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/mod.rs`（新增 helper）
- Modify: `crates/zeroclaw-gateway/src/lib.rs` 和 `src/ws.rs`（在 register_channels_for_tools 后调用 helper）

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-runtime/src/tools/mod.rs` 末尾的 `#[cfg(test)] mod` 内（如无则新建 `mod validate_dawn_task_tests`）追加：

```rust
#[cfg(test)]
mod validate_dawn_task_tests {
    use super::*;
    use zeroclaw_config::dawn_task::{DawnTaskExecutorConfig, DawnTaskExecutors};

    fn cfg_with_one_executor(channel_ref: &str) -> Config {
        let toml = format!(
            r#"
[dawn_task.1]
channel = "{channel_ref}"
recipient = "r"
name = "n"
description = "d"
"#
        );
        toml::from_str(&toml).expect("test toml parses")
    }

    // Sanity check: build empty Config to ensure the test fixture compiles.
    #[test]
    fn empty_config_parses() {
        let _cfg: Config = toml::from_str("").expect("empty toml parses");
    }

    #[test]
    fn validate_logs_no_warning_when_channel_present() {
        let cfg = cfg_with_one_executor("dawnim.work");
        let handle: PerToolChannelHandle = Arc::new(RwLock::new(std::collections::HashMap::from([
            ("dawnim.work".to_string(), placeholder_channel()),
        ])));
        // Should not panic; for now we just verify the call succeeds.
        validate_dawn_task_executors(&cfg, &handle);
    }

    #[test]
    fn validate_returns_missing_channel_refs() {
        let cfg = cfg_with_one_executor("wechat.missing");
        let handle: PerToolChannelHandle = Arc::new(RwLock::new(std::collections::HashMap::new()));
        let missing = validate_dawn_task_executors_collect(&cfg, &handle);
        assert_eq!(missing, vec!["wechat.missing".to_string()]);
    }

    fn placeholder_channel() -> Arc<dyn zeroclaw_api::channel::Channel> {
        use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

        struct Stub;
        impl zeroclaw_api::attribution::Attributable for Stub {
            fn attribution_role(&self) -> zeroclaw_api::attribution::Role {
                zeroclaw_api::attribution::Role::Channel(
                    zeroclaw_api::attribution::ChannelKind::Cli,
                )
            }
            fn attribution_alias(&self) -> Option<&str> { None }
        }
        #[async_trait::async_trait]
        impl Channel for Stub {
            fn name(&self) -> &str { "stub" }
            async fn send(&self, _: &SendMessage) -> anyhow::Result<()> { Ok(()) }
            async fn listen(
                &self,
                _: tokio::sync::mpsc::Sender<ChannelMessage>,
            ) -> anyhow::Result<()> { Ok(()) }
        }
        Arc::new(Stub)
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-runtime validate_dawn_task`
Expected: FAIL with "cannot find function `validate_dawn_task_executors`"

- [ ] **Step 3: 实现 validate_dawn_task_executors + collect 变体**

在 `crates/zeroclaw-runtime/src/tools/mod.rs` 中（与其他 helper 同区域）添加：

```rust
/// Sweep `[dawn_task.<n>]` executor configs against the populated channel
/// handle. Any `executor.channel` that doesn't match a registered channel
/// key emits a WARN log. Does not fail or block startup — surfaces config
/// typos / disabled channels early.
pub fn validate_dawn_task_executors(
    config: &Config,
    handle: &PerToolChannelHandle,
) {
    let missing = validate_dawn_task_executors_collect(config, handle);
    let map = handle.read();
    for ch_ref in missing {
        let task_keys: Vec<&String> = config
            .dawn_task
            .executors
            .iter()
            .filter(|(_, exec)| exec.channel == ch_ref)
            .map(|(k, _)| k)
            .collect();
        let available: Vec<&String> = map.keys().collect();
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "missing_channel": ch_ref,
                    "affected_task_types": task_keys,
                    "available_channels": available,
                })),
            "dawn_task: 配置的 channel 未注册或未启用，相关 task 将不可用"
        );
    }
}

/// Test-friendly variant: returns the list of missing channel refs instead
/// of logging them. `validate_dawn_task_executors` is the production caller.
pub fn validate_dawn_task_executors_collect(
    config: &Config,
    handle: &PerToolChannelHandle,
) -> Vec<String> {
    let map = handle.read();
    let mut missing: Vec<String> = config
        .dawn_task
        .executors
        .values()
        .map(|exec| exec.channel.clone())
        .filter(|ch_ref| !map.contains_key(ch_ref))
        .collect();
    missing.sort();
    missing.dedup();
    missing
}
```

- [ ] **Step 4: 在 gateway 调用点接线**

修改 `crates/zeroclaw-gateway/src/lib.rs` 和 `src/ws.rs`，在 `register_channels_for_tools(...)` 调用之后插入：

```rust
zeroclaw_runtime::tools::validate_dawn_task_executors(
    &config,
    &all_tools_result.task_channel_handle,
);
```

- [ ] **Step 5: 运行测试**

Run: `cargo test -p zeroclaw-runtime validate_dawn_task`
Expected: PASS (2 个测试)

- [ ] **Step 6: 全工作区编译 + 测试**

Run: `cargo check --workspace --all-targets`
Expected: 编译通过

Run: `cargo test -p dawn-tools -p zeroclaw-config -p zeroclaw-runtime --lib`
Expected: PASS

- [ ] **Step 7: 提交**

```bash
git add crates/zeroclaw-runtime/src/tools/mod.rs \
        crates/zeroclaw-gateway/src/lib.rs \
        crates/zeroclaw-gateway/src/ws.rs
git commit -m "feat(runtime): add startup validation for dawn_task executor channels"
```

---

## Task 12: 最终全工作区验证 + 更新 migration tracking 文档

**Files:**
- Modify: `docs/maintainers/migration-tracking-TBD.md`（更新 #39+#46 备注，新增"解耦设计"链接）

- [ ] **Step 1: 全工作区编译**

Run: `cargo fmt --all -- --check`
Expected: 无 diff

如有 fmt diff，运行 `cargo fmt --all` 修复并 stage：
```bash
cargo fmt --all
git add -u
```

- [ ] **Step 2: 全工作区 clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无 warning

如有 warning，按提示修复。

- [ ] **Step 3: 全工作区测试**

Run: `cargo test --workspace --lib`
Expected: PASS（pre-existing 不相关失败可忽略 — 与本次工作无关的失败需事先识别）

Run: `cargo test -p dawn-tools -p zeroclaw-config -p zeroclaw-api`
Expected: PASS

- [ ] **Step 4: 验证关键不变量**

Run 以下命令，确认输出为空（即解耦成功）：

```bash
grep -n "dawn-tools" crates/zeroclaw-channels/Cargo.toml
```
Expected: 无输出

```bash
grep -rn "dawn_tools::" crates/zeroclaw-channels/src/ 2>/dev/null
```
Expected: 无输出

```bash
grep -rn "CHANNEL_BRIDGE\|set_channel_bridge\|TaskMessage\|TaskContext\|TASK_CONTEXT" crates/dawn-tools/ crates/zeroclaw-channels/ 2>/dev/null
```
Expected: 无输出（除文档注释外）

```bash
grep -n "send_status_message" crates/zeroclaw-channels/src/dawn_im/channel.rs
```
Expected: 无输出

- [ ] **Step 5: 更新 migration tracking 文档**

修改 `docs/maintainers/migration-tracking-TBD.md`，在 #39+#46 的行追加链接：

把：
```
| **#39** | feat: xuanji-Dawn 文档提取集成 via WuKongIM bridge | ✅ 已迁移 | P3 | ... | 已迁移完成（与 #46 捆绑实施），架构升级：`parking_lot::RwLock` 替代 `OnceLock` 支持 hot reload；`TaskMessage` struct 替代裸 tuple；多 dawnim 别名路由；listener 每条消息独立 spawn 隔离失败 |
```

改为：
```
| **#39** | feat: xuanji-Dawn 文档提取集成 via WuKongIM bridge | ✅ 已迁移 | P3 | ... | 已迁移完成（与 #46 捆绑实施），后续通过 [dawn-tools 与 channel 解耦设计](../superpowers/specs/2026-06-14-dawn-tools-channel-decoupling-design.md) 演化为 `PerToolChannelHandle` 模式，消除 `zeroclaw-channels → dawn-tools` 反向依赖 |
```

#46 行同样追加链接。

- [ ] **Step 6: 提交**

```bash
git add docs/maintainers/migration-tracking-TBD.md
git commit -m "docs(tracking): link dawn-tools decoupling spec to #39/#46 migration notes"
```

- [ ] **Step 7: 验证 git 历史**

Run: `git log --oneline origin/0.8.0..HEAD`
Expected: 12 个新 commit（task 1-12），每个对应一个 task

---

## Spec Coverage Self-Check（写完后确认）

| Spec 章节 | 覆盖任务 |
|----------|---------|
| §5.1 SendKind / ChannelOrigin / CHANNEL_ORIGIN / SendMessage.kind | T1 / T2 / T3 |
| §5.2 channel.send 不动 trait shape + ensure_text_kind | T3 (helper) + T4 (DawnIM impl 自行 match) |
| §5.3 DawnTaskExecutors / DawnTaskExecutorConfig 重命名 + 加 channel/recipient | T5 |
| §5.4 Config.dawn_task 字段类型升级 | T5 |
| §6 运行时流程（CHANNEL_ORIGIN scope, executor lookup, send.kind 分发） | T8 (工具 execute 实现 + orchestrator 注入 ChannelOrigin) |
| §7 错误模式（4 类） | T8 (3 类工具内 Err) + T11 (启动期 WARN) |
| §8.1 zeroclaw-api 改动 | T1+T2+T3 |
| §8.2 zeroclaw-config 改动 | T5 |
| §8.3 zeroclaw-channels 改动 | T4 (send 分发) + T7 (register 加参数) + T8 (TaskContext→ChannelOrigin) + T9 (删 listener) + T10 (删 send_status_message + 删 dep) |
| §8.4 dawn-tools 改动 | T6 (handle 字段) + T8 (重写 execute) + T10 (删 bridge + 删 parking_lot) |
| §8.5 zeroclaw-runtime 改动 | T6 (AllToolsResult + factory) + T11 (validate_dawn_task_executors) |
| §8.6 register_channels_for_tools 调用点 | T7 (gateway lib.rs + ws.rs) |
| §10 兼容性 / 迁移 | T5 (用户 config 加 channel 字段) |
| §11 单一事实源 | T6 + T8 (handle 共享 + 工具持 Arc<Config> 快照与现状一致) |
| §12 验证计划 | T12 (cargo check + clippy + test) |
| §13 风险（含未审计 channel 行为） | ensure_text_kind helper T3 + 配置 gate by design |
