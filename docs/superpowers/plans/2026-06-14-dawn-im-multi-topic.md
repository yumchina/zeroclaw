# DawnIM 多话题映射 thread — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 DawnIM 协议层的 `topic` 字段自动映射到 ZeroClaw 既有的 `thread_ts` 机制，使同一 (channel, user) 下的多个 topic 拥有独立会话历史 + 记忆。

**Architecture:** 入站 `RecvNotificationParams.topic → ChannelMessage.thread_ts`（由 `topic_to_thread` helper 过滤 "0" / "" sentinel）；出站 `SendMessage.thread_ts → SendParams.topic + setting:Some(8u32)`；`ChannelOrigin` 扩展 `topic` 字段把当前 turn 的 topic 暴露给工具栈。

**Tech Stack:** Rust, tokio (task_local), serde, parking_lot::RwLock (现有)

**Spec:** [`docs/superpowers/specs/2026-06-14-dawn-im-multi-topic-design.md`](../specs/2026-06-14-dawn-im-multi-topic-design.md)

---

## File Structure

| 文件 | 职责 |
|------|------|
| `crates/zeroclaw-api/src/channel.rs` | **修改** `ChannelOrigin` struct 加 `topic: Option<String>` 字段 |
| `crates/zeroclaw-channels/src/dawn_im/connection.rs` | **修改** `RecvNotificationParams` + `SyncMessage` 各加 `topic: Option<String>` |
| `crates/zeroclaw-channels/src/dawn_im/channel.rs` | **修改** (1) 新增 `topic_to_thread` 私有 helper；(2) `process_inbound_message` 顶部计算 + 2 个 ChannelMessage 构造点填 thread_ts；(3) `Channel::send` `SendKind::Text` 分支映射；(4) `process_offline_batch` 按 topic 分组 + `send_offline_batch_as_single_message` 加 topic 参数 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | **修改** `process_channel_message_body` 构造 `ChannelOrigin` 加 `topic: msg.thread_ts.clone()` |

---

## Task 1: 给 `ChannelOrigin` 加 `topic` 字段

**Files:**
- Modify: `crates/zeroclaw-api/src/channel.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-api/src/channel.rs` 末尾的测试模块（找 `mod channel_origin_tests`）追加：

```rust
    #[test]
    fn channel_origin_default_topic_is_none() {
        let o = ChannelOrigin::default();
        assert!(o.topic.is_none());
    }

    #[tokio::test]
    async fn channel_origin_scope_carries_topic() {
        let origin = ChannelOrigin {
            from_uid: "u_alice".into(),
            channel_ref: "dawnim.work".into(),
            reply_target: "1:u_alice".into(),
            topic: Some("db_lock".into()),
        };
        let read_back = CHANNEL_ORIGIN
            .scope(origin.clone(), async {
                CHANNEL_ORIGIN.try_with(|o| o.topic.clone()).unwrap()
            })
            .await;
        assert_eq!(read_back, Some("db_lock".to_string()));
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-api channel_origin_default_topic_is_none channel_origin_scope_carries_topic`
Expected: FAIL — `ChannelOrigin` 没有 `topic` 字段

- [ ] **Step 3: 加字段到 ChannelOrigin**

在 `crates/zeroclaw-api/src/channel.rs` 找到 `pub struct ChannelOrigin { ... }`，在 `reply_target` 字段之后追加：

```rust
    /// Per-turn topic identifier. `None` means "no topic" (default
    /// behaviour, equivalent to pre-multi-topic single-thread session).
    /// `Some(t)` means the current turn lives in the isolated topic `t`
    /// — its conversation history and memory are scoped separately from
    /// other topics under the same (channel, user) pair.
    ///
    /// Sourced from the inbound `ChannelMessage.thread_ts` by the
    /// orchestrator. Channel-aware tools read this to make topic-aware
    /// decisions; the default tools (e.g. `dawn_create_task`) currently
    /// ignore it (task messages route to external agent UIDs, not topic-
    /// scoped user sessions).
    pub topic: Option<String>,
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-api channel_origin`
Expected: PASS (含先前 3 个 + 新增 2 个，共 5 个)

- [ ] **Step 5: 全工作区编译验证**

Run: `cargo check --workspace --all-targets 2>&1 | tail -10`
Expected: 编译通过。**预期**会有 `missing field 'topic' in initializer of ChannelOrigin` 错误 —— orchestrator 构造点（line ~4678）需要 Task 7 填上。**本 task 不修复**那个错误（留给 Task 7），但记录下来。

如果非 orchestrator 还有其它 ChannelOrigin 字面量构造点（grep `ChannelOrigin {` 找），那些需要补 `topic: None,` —— 因为只有 orchestrator 有真实的 thread_ts 来源，其它地方（如测试 stub）填 None 即可。

Run: `grep -rn "ChannelOrigin {" --include="*.rs" crates/ 2>/dev/null`
对每个返回的位置：如果不是 orchestrator 的 `process_channel_message_body` 中那个（line 4678 附近），就补 `topic: None,`。

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-api/src/channel.rs
# 如果 Step 5 修了其它文件，一并 add
git commit -m "feat(api): add ChannelOrigin.topic for per-turn topic context"
```

**NO `Co-Authored-By` trailer.** 严格 ZeroClaw 政策。

---

## Task 2: `RecvNotificationParams` + `SyncMessage` 加 `topic` 字段

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/connection.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-channels/src/dawn_im/connection.rs` 末尾追加（如已有测试模块就追加；否则新建）：

```rust
#[cfg(test)]
mod topic_field_tests {
    use super::*;

    #[test]
    fn recv_params_parses_topic_when_present() {
        let json = r#"{
            "messageId": "m1",
            "messageSeq": 1,
            "fromUid": "u_alice",
            "channelId": "u_alice",
            "channelType": 1,
            "payload": {},
            "timestamp": 0,
            "topic": "db_lock"
        }"#;
        let parsed: RecvNotificationParams = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.topic.as_deref(), Some("db_lock"));
    }

    #[test]
    fn recv_params_topic_defaults_to_none_when_missing() {
        let json = r#"{
            "messageId": "m1",
            "messageSeq": 1,
            "fromUid": "u_alice",
            "channelId": "u_alice",
            "channelType": 1,
            "payload": {},
            "timestamp": 0
        }"#;
        let parsed: RecvNotificationParams = serde_json::from_str(json).unwrap();
        assert!(parsed.topic.is_none());
    }

    #[test]
    fn sync_message_parses_topic() {
        let json = r#"{
            "message_id": "m1",
            "message_seq": 1,
            "from_uid": "u_alice",
            "payload": {},
            "timestamp": 0,
            "topic": "db_lock"
        }"#;
        let parsed: SyncMessage = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.topic.as_deref(), Some("db_lock"));
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM topic_field 2>&1 | tail -10`
Expected: FAIL — `RecvNotificationParams` / `SyncMessage` 没有 `topic` 字段

- [ ] **Step 3: 加字段到 RecvNotificationParams**

在 `crates/zeroclaw-channels/src/dawn_im/connection.rs` 找到 `pub struct RecvNotificationParams { ... }`，在 `pub timestamp: i64,` 之后追加：

```rust
    /// DawnIM logical topic identifier. `None` / `Some("")` / `Some("0")`
    /// all mean "no topic" (default thread). Any other value is treated
    /// as an isolated topic and mapped to `ChannelMessage.thread_ts`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
```

- [ ] **Step 4: 加字段到 SyncMessage**

在同文件找到 `pub struct SyncMessage { ... }`，在 `pub timestamp: i64,` 之后追加：

```rust
    /// DawnIM logical topic identifier (offline sync). See
    /// `RecvNotificationParams.topic` for semantics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
```

- [ ] **Step 5: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM topic_field 2>&1 | tail -10`
Expected: PASS (3 个)

- [ ] **Step 6: 全工作区编译**

Run: `cargo check --workspace --all-targets --features channel-dawnIM 2>&1 | tail -5`
Expected: 编译通过

- [ ] **Step 7: 提交**

```bash
git add crates/zeroclaw-channels/src/dawn_im/connection.rs
git commit -m "feat(channels/dawn_im): protocol topic field on RecvNotificationParams + SyncMessage"
```

---

## Task 3: `topic_to_thread` helper + unit tests

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 末尾追加（新增测试模块 `topic_to_thread_tests`）：

```rust
#[cfg(test)]
mod topic_to_thread_tests {
    use super::topic_to_thread;

    #[test]
    fn none_maps_to_none() {
        assert_eq!(topic_to_thread(None), None);
    }

    #[test]
    fn empty_string_maps_to_none() {
        assert_eq!(topic_to_thread(Some("")), None);
    }

    #[test]
    fn zero_sentinel_maps_to_none() {
        assert_eq!(topic_to_thread(Some("0")), None);
    }

    #[test]
    fn real_topic_maps_to_some() {
        assert_eq!(topic_to_thread(Some("db_lock")), Some("db_lock".to_string()));
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM topic_to_thread 2>&1 | tail -8`
Expected: FAIL — `topic_to_thread` 未定义

- [ ] **Step 3: 加 helper**

在 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 中合适位置（建议靠文件顶部，紧邻 imports 之后，在 `pub struct DawnIMChannel` 定义之前）插入：

```rust
/// Normalise a DawnIM `topic` field into an Option<String> suitable for
/// `ChannelMessage.thread_ts`. The DawnIM protocol uses `"0"` and `""`
/// as sentinels for "no topic"; both map to `None` so historical
/// single-thread conversations get the legacy session key unchanged.
fn topic_to_thread(topic: Option<&str>) -> Option<String> {
    topic
        .filter(|t| !t.is_empty() && *t != "0")
        .map(ToString::to_string)
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM topic_to_thread 2>&1 | tail -8`
Expected: PASS (4 个)

- [ ] **Step 5: 提交**

```bash
git add crates/zeroclaw-channels/src/dawn_im/channel.rs
git commit -m "feat(channels/dawn_im): add topic_to_thread normaliser helper"
```

---

## Task 4: Inbound mapping — `process_inbound_message` 把 topic 写入 `ChannelMessage.thread_ts`

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`

**说明**：当前函数有 **2 个** `ChannelMessage` 构造点（line 652 CMD path、line 788 main message path），都填 `thread_ts: None`。把它们改为读 inbound topic。

- [ ] **Step 1: 写失败测试**

由于 process_inbound_message 涉及 mpsc + WS 模拟，做完整 e2e 测试代价高。采用**间接验证**：写一个单元测试直接验证 ChannelMessage 出现 thread_ts 的行为。改追加到现有的 `topic_to_thread_tests` 模块或新建测试。

在 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 末尾追加：

```rust
#[cfg(test)]
mod inbound_topic_mapping_tests {
    use super::*;
    use tokio::sync::mpsc;

    fn channel_with_test_state() -> DawnIMChannel {
        let cfg = zeroclaw_config::schema::DawnIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".into(),
            uid: "bot_uid_1".into(),
            token: String::new(),
            device_id: "test-device".into(),
            allowed_users: vec!["*".into()],
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn zeroclaw_api::memory_traits::Memory> = Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", tmp.path(), "inbound_topic_test")
                .unwrap(),
        );
        DawnIMChannel::from_config(&cfg, "test", tmp.path(), memory)
    }

    fn make_text_recv(topic: Option<&str>) -> RecvNotificationParams {
        let payload = serde_json::json!({"type": 1, "content": "hello"});
        let payload_b64 = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_string(&payload).unwrap());
        RecvNotificationParams {
            message_id: "m1".into(),
            message_seq: 1,
            from_uid: "u_alice".into(),
            channel_id: "u_alice".into(),
            channel_type: WkChannelType::PERSONAL,
            payload: serde_json::Value::String(payload_b64),
            timestamp: 1,
            topic: topic.map(ToString::to_string),
        }
    }

    #[tokio::test]
    async fn inbound_text_with_topic_sets_thread_ts() {
        let ch = channel_with_test_state();
        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(8);
        ch.process_inbound_message(make_text_recv(Some("db_lock")), &tx)
            .await
            .unwrap();
        let msg = rx.recv().await.expect("text message delivered");
        assert_eq!(msg.thread_ts.as_deref(), Some("db_lock"));
    }

    #[tokio::test]
    async fn inbound_text_without_topic_keeps_thread_ts_none() {
        let ch = channel_with_test_state();
        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(8);
        ch.process_inbound_message(make_text_recv(None), &tx)
            .await
            .unwrap();
        let msg = rx.recv().await.expect("text message delivered");
        assert!(msg.thread_ts.is_none());
    }

    #[tokio::test]
    async fn inbound_text_with_zero_sentinel_keeps_thread_ts_none() {
        let ch = channel_with_test_state();
        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(8);
        ch.process_inbound_message(make_text_recv(Some("0")), &tx)
            .await
            .unwrap();
        let msg = rx.recv().await.expect("text message delivered");
        assert!(msg.thread_ts.is_none());
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM inbound_topic 2>&1 | tail -15`
Expected: FAIL — `inbound_text_with_topic_sets_thread_ts` 应该失败（产出的 ChannelMessage.thread_ts 还是 None）。其它两个可能通过（因为本来就是 None）。

- [ ] **Step 3: 在 process_inbound_message 顶部计算 `topic_thread`**

找到 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 中 `async fn process_inbound_message(&self, params: RecvNotificationParams, tx: &tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {` —— 在函数体最顶部（即开第一个 `{` 后的第一行，先于任何其他逻辑）加：

```rust
        let topic_thread = topic_to_thread(params.topic.as_deref());
```

- [ ] **Step 4: 替换 CMD path 的 thread_ts**

找到原 line 652 附近（CMD path `la_init_helloworld` 的 ChannelMessage 构造点）。把：

```rust
                    thread_ts: None,
```

改为：

```rust
                    thread_ts: topic_thread.clone(),
```

- [ ] **Step 5: 替换 main message path 的 thread_ts**

找到原 line 788 附近（main message path 的 ChannelMessage 构造点）。把：

```rust
            thread_ts: None,
```

改为：

```rust
            thread_ts: topic_thread.clone(),
```

- [ ] **Step 6: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM inbound_topic 2>&1 | tail -10`
Expected: PASS (3 个)

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM dawn_im 2>&1 | tail -5`
Expected: 全部 dawn_im 测试仍通过（行为兼容回归）

- [ ] **Step 7: 提交**

```bash
git add crates/zeroclaw-channels/src/dawn_im/channel.rs
git commit -m "feat(channels/dawn_im): inbound topic maps to ChannelMessage.thread_ts"
```

---

## Task 5: Outbound mapping — `Channel::send` `SendKind::Text` 分支映射

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/zeroclaw-channels/src/dawn_im/channel.rs` 末尾追加：

```rust
#[cfg(test)]
mod outbound_topic_mapping_tests {
    use super::*;
    use zeroclaw_api::channel::{SendKind, SendMessage};

    fn channel_with_test_state() -> DawnIMChannel {
        let cfg = zeroclaw_config::schema::DawnIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".into(),
            uid: "bot_uid_1".into(),
            token: String::new(),
            device_id: "test-device".into(),
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn zeroclaw_api::memory_traits::Memory> = Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", tmp.path(), "outbound_topic_test")
                .unwrap(),
        );
        DawnIMChannel::from_config(&cfg, "test", tmp.path(), memory)
    }

    /// Verify outbound mapping by intercepting the SendParams that would
    /// be sent. We can't easily test the full WS path; verify via the helper
    /// we factor out instead. See `build_text_send_params`.
    #[test]
    fn outbound_text_with_thread_ts_sets_topic_and_setting_bit() {
        let ch = channel_with_test_state();
        let msg = SendMessage {
            content: "hello".into(),
            recipient: "1:u_alice".into(),
            thread_ts: Some("db_lock".into()),
            kind: SendKind::Text,
            ..Default::default()
        };
        let params = ch.build_text_send_params(&msg).unwrap();
        assert_eq!(params.topic.as_deref(), Some("db_lock"));
        assert_eq!(params.setting, Some(8u32));
    }

    #[test]
    fn outbound_text_without_thread_ts_keeps_topic_and_setting_none() {
        let ch = channel_with_test_state();
        let msg = SendMessage {
            content: "hello".into(),
            recipient: "1:u_alice".into(),
            thread_ts: None,
            kind: SendKind::Text,
            ..Default::default()
        };
        let params = ch.build_text_send_params(&msg).unwrap();
        assert!(params.topic.is_none());
        assert!(params.setting.is_none());
    }

    #[test]
    fn outbound_text_with_zero_sentinel_keeps_topic_and_setting_none() {
        let ch = channel_with_test_state();
        let msg = SendMessage {
            content: "hello".into(),
            recipient: "1:u_alice".into(),
            thread_ts: Some("0".into()),
            kind: SendKind::Text,
            ..Default::default()
        };
        let params = ch.build_text_send_params(&msg).unwrap();
        assert!(params.topic.is_none());
        assert!(params.setting.is_none());
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM outbound_topic 2>&1 | tail -10`
Expected: FAIL — `build_text_send_params` 不存在

- [ ] **Step 3: 抽出 build_text_send_params helper**

在 `DawnIMChannel` 的 `impl` 块中（与 `send_task_payload` 同区域，相同 `impl DawnIMChannel` 块内），新增私有 helper。

找到 `Channel::send` 中 `SendKind::Text => { ... }` 分支（约 line 1086-1110 区域）内部的 SendParams 构造代码：

```rust
                let payload_b64 = if let Some(code) = message.content.strip_prefix("ERR:") {
                    let card = build_exception_card(code);
                    base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(&card)?)
                } else {
                    encode_text_payload(&message.content)?
                };
                let (channel_id, channel_type) = parse_recipient(&message.recipient);
                let params = SendParams {
                    from_uid: Some(self.uid.clone()),
                    client_msg_no: Uuid::new_v4().to_string(),
                    channel_id,
                    channel_type,
                    payload: serde_json::Value::String(payload_b64),
                    header: None,
                    setting: None,
                    msg_key: None,
                    expire: None,
                    stream_no: None,
                    topic: None,
                };
```

把 SendParams 构造逻辑提到一个新私有 fn：

```rust
    /// Construct `SendParams` for a `SendKind::Text` outbound message,
    /// including the DawnIM topic mapping (`SendMessage.thread_ts` →
    /// `SendParams.topic` + `setting` bit-3 to flag topic presence to the
    /// DawnIM server). Topic sentinels `""` and `"0"` are filtered to
    /// `None` via `topic_to_thread`.
    fn build_text_send_params(
        &self,
        message: &zeroclaw_api::channel::SendMessage,
    ) -> anyhow::Result<SendParams> {
        let payload_b64 = if let Some(code) = message.content.strip_prefix("ERR:") {
            let card = build_exception_card(code);
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_string(&card)?)
        } else {
            encode_text_payload(&message.content)?
        };
        let (channel_id, channel_type) = parse_recipient(&message.recipient);
        let topic_out = topic_to_thread(message.thread_ts.as_deref());
        let setting_out: Option<u32> = topic_out.as_ref().map(|_| 8u32);
        Ok(SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id,
            channel_type,
            payload: serde_json::Value::String(payload_b64),
            header: None,
            setting: setting_out,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: topic_out,
        })
    }
```

- [ ] **Step 4: 替换 SendKind::Text 分支调用 helper**

把原 SendKind::Text 分支中的整个 `let params = SendParams { ... };` 语句替换为：

```rust
                let params = self.build_text_send_params(message)?;
```

保留 helper 之后的 `let mut g = self.ws_sink.write().await;` 和后续 WS 发送逻辑不变。

- [ ] **Step 5: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM outbound_topic 2>&1 | tail -10`
Expected: PASS (3 个)

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM dawn_im 2>&1 | tail -5`
Expected: 全部 dawn_im 测试仍通过

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-channels/src/dawn_im/channel.rs
git commit -m "feat(channels/dawn_im): outbound SendKind::Text maps thread_ts to topic + setting bit"
```

---

## Task 6: Offline batch — 按 topic 分组

**Files:**
- Modify: `crates/zeroclaw-channels/src/dawn_im/channel.rs`

**说明**：当前 `process_offline_batch` 把所有 offline 消息合并成单条 ChannelMessage（line 1020-1036），导致跨 topic 消息被混在一起。需按 topic 分组，每组生成独立 ChannelMessage。

- [ ] **Step 1: 写失败测试**

末尾追加：

```rust
#[cfg(test)]
mod offline_batch_topic_grouping_tests {
    use super::*;
    use tokio::sync::mpsc;

    fn channel_with_test_state() -> DawnIMChannel {
        let cfg = zeroclaw_config::schema::DawnIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".into(),
            uid: "bot_uid_1".into(),
            token: String::new(),
            device_id: "test-device".into(),
            allowed_users: vec!["*".into()],
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let memory: Arc<dyn zeroclaw_api::memory_traits::Memory> = Arc::new(
            zeroclaw_memory::SqliteMemory::new_named("sqlite", tmp.path(), "offline_topic_test")
                .unwrap(),
        );
        DawnIMChannel::from_config(&cfg, "test", tmp.path(), memory)
    }

    fn make_recv(seq: u32, ts: i64, topic: Option<&str>, text: &str) -> RecvNotificationParams {
        let payload = serde_json::json!({"type": 1, "content": text});
        let payload_b64 = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_string(&payload).unwrap());
        RecvNotificationParams {
            message_id: format!("m{seq}"),
            message_seq: seq,
            from_uid: "u_alice".into(),
            channel_id: "u_alice".into(),
            channel_type: WkChannelType::PERSONAL,
            payload: serde_json::Value::String(payload_b64),
            timestamp: ts,
            topic: topic.map(ToString::to_string),
        }
    }

    #[tokio::test]
    async fn offline_batch_with_mixed_topics_splits_into_separate_channel_messages() {
        let ch = channel_with_test_state();
        let (tx, mut rx) = mpsc::channel::<ChannelMessage>(8);
        // 3 from topic A, 2 from no-topic, 1 from topic B → expect 3 batches
        let batch = vec![
            make_recv(1, 1, Some("A"), "a1"),
            make_recv(2, 2, None,      "n1"),
            make_recv(3, 3, Some("A"), "a2"),
            make_recv(4, 4, Some("0"), "n2"), // sentinel — same group as None
            make_recv(5, 5, Some("B"), "b1"),
            make_recv(6, 6, Some("A"), "a3"),
        ];
        ch.process_offline_batch(batch, &tx).await.unwrap();
        drop(tx); // close so rx ends after draining

        let mut by_thread: std::collections::HashMap<Option<String>, ChannelMessage> =
            std::collections::HashMap::new();
        while let Some(msg) = rx.recv().await {
            by_thread.insert(msg.thread_ts.clone(), msg);
        }
        assert_eq!(by_thread.len(), 3, "expected 3 groups; got {by_thread:?}");
        assert!(by_thread.contains_key(&Some("A".to_string())));
        assert!(by_thread.contains_key(&Some("B".to_string())));
        assert!(by_thread.contains_key(&None));

        // Group A should contain a1/a2/a3 in order
        let a = by_thread.get(&Some("A".to_string())).unwrap();
        assert!(a.content.contains("a1"));
        assert!(a.content.contains("a2"));
        assert!(a.content.contains("a3"));
        // No-topic group should contain n1/n2
        let none = by_thread.get(&None).unwrap();
        assert!(none.content.contains("n1"));
        assert!(none.content.contains("n2"));
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM offline_batch_topic 2>&1 | tail -10`
Expected: FAIL — 当前 batch 合并为 1 条 ChannelMessage，by_thread.len() == 1

- [ ] **Step 3: 重构 process_offline_batch — 按 topic 分组**

找到 `process_offline_batch` 函数。当前末尾的：

```rust
        self.send_offline_batch_as_single_message(sorted_messages, is_silent, tx)
            .await?;

        self.clear_unread(&channel_id, channel_type, last_seq)
            .await?;
        Ok(())
    }
```

替换 `send_offline_batch_as_single_message` 调用为按 topic 分组 + 循环发送：

```rust
        // Group by topic — different topics get separate ChannelMessages
        // so their session histories / memories stay isolated. Order
        // within a topic is preserved (sorted_messages is already sorted
        // by timestamp).
        let mut by_topic: std::collections::HashMap<Option<String>, Vec<RecvNotificationParams>> =
            std::collections::HashMap::new();
        for m in sorted_messages {
            let topic = topic_to_thread(m.topic.as_deref());
            by_topic.entry(topic).or_default().push(m);
        }

        for (topic_thread, group) in by_topic {
            // is_silent is recomputed per topic group: a mention in topic A
            // shouldn't suppress topic B's silent flag.
            let group_silent = if is_group && self.mention_only {
                let mut has_mention = false;
                for m in &group {
                    let payload_json: serde_json::Value = if m.payload.is_string() {
                        base64::engine::general_purpose::STANDARD
                            .decode(m.payload.as_str().unwrap_or_default())
                            .ok()
                            .and_then(|b| serde_json::from_slice(&b).ok())
                            .unwrap_or_default()
                    } else {
                        m.payload.clone()
                    };
                    let content = payload_json
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    if is_mentioned(&self.uid, &payload_json, content) {
                        has_mention = true;
                        break;
                    }
                }
                !has_mention
            } else {
                false
            };
            self.send_offline_batch_as_single_message(group, topic_thread, group_silent, tx)
                .await?;
        }

        self.clear_unread(&channel_id, channel_type, last_seq)
            .await?;
        Ok(())
    }
```

**Important**: 这个替换**删除**了原函数中部预先计算的 `is_silent` 逻辑（line 893-918）以及它后面的 `record!` 日志（line 920-931）。因为现在 silent 是 per-topic 算的。

具体步骤：找到原代码：

```rust
        let is_silent = if is_group && self.mention_only {
            let mut has_mention = false;
            ...（约 25 行 mention check）
            !has_mention
        } else {
            false
        };

        ::zeroclaw_log::record!(
            INFO,
            ...（offline batch 日志）
        );

        self.send_offline_batch_as_single_message(sorted_messages, is_silent, tx)
            .await?;
```

整段替换为（保留 batch-level 日志，但去掉预算的 is_silent）：

```rust
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "channel_id": channel_id,
                    "channel_type": channel_type,
                    "count": sorted_messages.len(),
                })
            ),
            "DawnIM: processing offline batch"
        );

        // Group by topic — different topics get separate ChannelMessages
        // so their session histories / memories stay isolated. Order
        // within a topic is preserved (sorted_messages is already sorted
        // by timestamp).
        let mut by_topic: std::collections::HashMap<Option<String>, Vec<RecvNotificationParams>> =
            std::collections::HashMap::new();
        for m in sorted_messages {
            let topic = topic_to_thread(m.topic.as_deref());
            by_topic.entry(topic).or_default().push(m);
        }

        for (topic_thread, group) in by_topic {
            // is_silent is recomputed per topic group: a mention in topic A
            // shouldn't suppress topic B's silent flag.
            let group_silent = if is_group && self.mention_only {
                let mut has_mention = false;
                for m in &group {
                    let payload_json: serde_json::Value = if m.payload.is_string() {
                        base64::engine::general_purpose::STANDARD
                            .decode(m.payload.as_str().unwrap_or_default())
                            .ok()
                            .and_then(|b| serde_json::from_slice(&b).ok())
                            .unwrap_or_default()
                    } else {
                        m.payload.clone()
                    };
                    let content = payload_json
                        .get("content")
                        .and_then(|c| c.as_str())
                        .unwrap_or_default();
                    if is_mentioned(&self.uid, &payload_json, content) {
                        has_mention = true;
                        break;
                    }
                }
                !has_mention
            } else {
                false
            };
            self.send_offline_batch_as_single_message(group, topic_thread, group_silent, tx)
                .await?;
        }
```

- [ ] **Step 4: 加 `topic_thread` 参数到 `send_offline_batch_as_single_message`**

找到 `async fn send_offline_batch_as_single_message(...)` 签名（约 line 941）：

```rust
    async fn send_offline_batch_as_single_message(
        &self,
        messages: Vec<RecvNotificationParams>,
        silent: bool,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
```

改为：

```rust
    async fn send_offline_batch_as_single_message(
        &self,
        messages: Vec<RecvNotificationParams>,
        topic_thread: Option<String>,
        silent: bool,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
```

并在函数内部 ChannelMessage 构造点（约 line 1032）把 `thread_ts: None,` 改为 `thread_ts: topic_thread.clone(),`。

- [ ] **Step 5: 运行测试，确认通过**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM offline_batch_topic 2>&1 | tail -10`
Expected: PASS

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM dawn_im 2>&1 | tail -5`
Expected: 全部 dawn_im 测试仍通过

- [ ] **Step 6: 全工作区编译**

Run: `cargo check --workspace --all-targets --features channel-dawnIM 2>&1 | tail -5`
Expected: 编译通过

- [ ] **Step 7: 提交**

```bash
git add crates/zeroclaw-channels/src/dawn_im/channel.rs
git commit -m "feat(channels/dawn_im): split offline batch by topic for isolated ChannelMessages"
```

---

## Task 7: Orchestrator — 填 `ChannelOrigin.topic`

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1: 写失败测试**

orchestrator 已有 `process_channel_message` 的集成测试。不直接写新测试 —— 而是依靠**编译错误**驱动 (Task 1 Step 5 已经记下 orchestrator 这里有 `missing field 'topic'` 编译错误)。

如果想要显式测试，可以在 `process_channel_message_body` 附近测试段加：

```rust
    #[tokio::test]
    async fn channel_origin_carries_topic_from_message_thread_ts() {
        // Construct a ChannelMessage with thread_ts = Some("db_lock"),
        // verify the CHANNEL_ORIGIN scoped during processing has matching topic.
        // (Use the existing peer_prompt_test_context or similar helper)
        // ... 详见 orchestrator 现有测试模式，借用一个最小化的 process_channel_message 调用 ...
        // 由于 orchestrator 测试基础设施重，此测试可选；本 task 主依赖编译错误驱动
    }
```

**简化版本**：本 task 不强求新增 orchestrator 测试，依赖编译错误 + 后续集成测试（Task 8）端到端验证。

- [ ] **Step 2: 找到 `ChannelOrigin` 构造点**

Run: `grep -n "let channel_origin" crates/zeroclaw-channels/src/orchestrator/mod.rs`
Expected: 约 line 4678 一处。

- [ ] **Step 3: 加 `topic` 字段**

当前代码：

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

在 struct 字面量末尾（`channel_ref` 之后）加：

```rust
        topic: msg.thread_ts.clone(),
```

最终：

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
        topic: msg.thread_ts.clone(),
    };
```

- [ ] **Step 4: 全工作区编译**

Run: `cargo check --workspace --all-targets --features channel-dawnIM 2>&1 | tail -5`
Expected: 编译通过 — Task 1 Step 5 标记的 `missing field 'topic'` 错误现在消除

- [ ] **Step 5: 跑相关测试**

Run: `cargo test -p zeroclaw-channels --features channel-dawnIM process_channel_message 2>&1 | tail -10`
Expected: 既有 process_channel_message 测试全过（行为不变 — 旧消息 thread_ts 是 None 时 topic 也是 None）

- [ ] **Step 6: 提交**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(orchestrator): populate ChannelOrigin.topic from msg.thread_ts"
```

---

## Task 8: 最终全工作区验证 + 端到端 sanity check

**Files:**
- 无新增；可能根据 fmt 输出修小问题

- [ ] **Step 1: fmt check**

Run: `cargo fmt -p zeroclaw-api -p zeroclaw-channels -- --check 2>&1 | tail -5`

If diff exists in our touched files:
```bash
cargo fmt -p zeroclaw-api -p zeroclaw-channels
git add crates/zeroclaw-api/src/channel.rs crates/zeroclaw-channels/src/dawn_im/connection.rs crates/zeroclaw-channels/src/dawn_im/channel.rs crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "style: apply cargo fmt to multi-topic touched files"
```

- [ ] **Step 2: clippy 严格**

Run: `cargo clippy -p zeroclaw-api -p zeroclaw-channels --features channel-dawnIM -- -D warnings 2>&1 | tail -15`
Expected: 0 errors。如有新 warning，按提示修复。

- [ ] **Step 3: 完整测试**

Run: `cargo test -p zeroclaw-api -p zeroclaw-channels --features channel-dawnIM 2>&1 | tail -15`
Expected: 所有新增测试 + 既有测试通过。预先识别 pre-existing 失败不在本 task 范围。

- [ ] **Step 4: 关键 invariant grep**

Run: `grep -n "thread_ts: None" crates/zeroclaw-channels/src/dawn_im/channel.rs`
Expected: 0 occurrence —— 所有 ChannelMessage 构造点都已改为读 topic_thread

Run: `grep -n "topic: None" crates/zeroclaw-channels/src/dawn_im/channel.rs`
Expected: 仅出现在 `SendKind::TaskSubmit` / `TaskQuery` 分支的 SendParams（task 路径不绑 topic），以及 `setting_out` 为 None 时构造的 SendParams（这部分通过 build_text_send_params 的 topic_out 变量名替换，应已消失；double check）

Run: `grep -n "build_text_send_params" crates/zeroclaw-channels/src/dawn_im/channel.rs`
Expected: 定义 1 处 + 调用 1 处 = 2 行

Run: `grep -n "ChannelOrigin.*topic" crates/zeroclaw-channels/src/orchestrator/mod.rs`
Expected: 至少 1 行（构造 ChannelOrigin 时）

- [ ] **Step 5: （可选）docs/maintainers/migration-tracking-TBD.md 更新**

更新 PR #45 的状态 / 最终结论列：

```
| #45 | ... | ✅ 已迁移 | P6 → 完成 | ... | 已通过 [DawnIM 多话题映射 thread 设计](../superpowers/specs/2026-06-14-dawn-im-multi-topic-design.md) + [实施计划](../superpowers/plans/2026-06-14-dawn-im-multi-topic.md) 重新设计完成；不照搬原 PR (跳过 SettingFlags 重构)，新增 ChannelOrigin.topic 暴露给工具栈 |
```

```bash
git add docs/maintainers/migration-tracking-TBD.md
git commit -m "docs(tracking): mark PR #45 migration complete via multi-topic redesign"
```

---

## Spec Coverage Self-Check

| Spec 章节 | 覆盖任务 |
|----------|---------|
| §5.1 ChannelOrigin.topic 字段 | T1 |
| §5.2 RecvNotificationParams.topic / SyncMessage.topic | T2 |
| §5.3 topic_to_thread helper | T3 |
| §5.3 入站 process_inbound_message 映射 | T4 |
| §5.3 出站 Channel::send SendKind::Text 映射 | T5 |
| §5.3 offline batch 按 topic 分组 | T6 |
| §5.4 orchestrator ChannelOrigin.topic 填充 | T7 |
| §10 兼容性（旧消息 thread_ts None 不变） | T4 / T5 / T6 测试中覆盖 |
| §12 验证计划 | T8 |
| §13 风险（topic sentinel 集中处理） | topic_to_thread helper 集中（T3） |
