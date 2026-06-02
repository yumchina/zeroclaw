# WuKongIM 离线消息批处理过滤器实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 WuKongIM 离线消息同步批处理机制中，过滤掉所有属于助手自身（`bot`）的消息以及系统指令/状态指示等非普通会话消息，以确保 Orchestrator 的 LLM 回复决策不会被干扰判定为 `NoReply`，并保证即使过滤后批次为空也能正确原子推进同步位点。

**Architecture:** 
1. 在 `WuKongIMChannel::process_offline_batch` 中对 `sorted_messages` 引入以消息发送人、消息类型 (CMD, InteractiveCard, InteractiveResponse, StatusUpdate) 为筛选基准的过滤逻辑。
2. 若过滤后的有效消息列表为空，则静默更新同步状态点并清理未读，不进行任何 combined 拼接及向下流转。
3. 若有效消息列表非空，则正常将净化后的消息、最终位点序列及时间戳向下传递至组合及发送逻辑，安全推进状态。

**Tech Stack:** Rust 2024, tokio, serde_json, tracing

---

## 文件结构

**修改：**
- `crates/zeroclaw-channel-wukongim/src/channel.rs` —— 核心离线消息批处理及过滤机制

---

## Task 1：重构离线消息批处理过滤与流转机制

**文件：**
- 修改：`crates/zeroclaw-channel-wukongim/src/channel.rs`

- [ ] **Step 1：在 `process_offline_batch` 内引入基于 `filtered_messages` 过滤逻辑**

对 `crates/zeroclaw-channel-wukongim/src/channel.rs:658` 开始的 `process_offline_batch` 函数进行重构，在排序后，通过对 `sorted_messages` 进行过滤筛选，提取 `filtered_messages` 集合：

```rust
        // Filter out bot's own messages and system/non-conversational messages
        let filtered_messages: Vec<RecvNotificationParams> = sorted_messages
            .iter()
            .filter(|m| {
                if m.from_uid == self.uid {
                    return false;
                }

                let payload_json: serde_json::Value = if m.payload.is_string() {
                    base64::engine::general_purpose::STANDARD
                        .decode(m.payload.as_str().unwrap_or_default())
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or_default()
                } else {
                    m.payload.clone()
                };

                let msg_type = payload_json.get("type").and_then(|t| t.as_u64()).unwrap_or(0);
                if msg_type == WkMessageType::CMD as u64 || payload_json.get("cmd").is_some() {
                    return false;
                }
                if msg_type == WkMessageType::INTERACTIVE_RESPONSE as u64 {
                    return false;
                }
                if msg_type == WkMessageType::INTERACTIVE_CARD as u64 {
                    return false;
                }
                if msg_type == 23 {
                    return false;
                }

                true
            })
            .cloned()
            .collect();
```

- [ ] **Step 2：对空批次过滤结果进行分流与静默推进**

在过滤操作后，新增对 `filtered_messages.is_empty()` 的判断。若为空，表示全部为系统或卡片自更新，不需要发送：

```rust
        if filtered_messages.is_empty() {
            tracing::info!(
                "WuKongIM: offline batch channel={}:{} filtered to empty, updating sync state to seq={}",
                channel_id,
                channel_type,
                last_seq
            );
            self.update_sync_state(
                &channel_id,
                channel_type,
                last_seq,
                last.timestamp * 1_000_000_000,
            )
            .await?;
            let _ = self.clear_unread(&channel_id, channel_type, last_seq).await;
            return Ok(());
        }
```

- [ ] **Step 3：调整 `send_offline_batch_as_single_message` 参数及状态推进流转**

更新 `send_offline_batch_as_single_message` 签名以接收 `last_seq` 和 `last_timestamp_ns`，以便在消息发送成功后原子化更新位点，防止在上游直接修改引起的状态竞态：

```rust
    async fn send_offline_batch_as_single_message(
        &self,
        messages: Vec<RecvNotificationParams>,
        silent: bool,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
        last_seq: u32,
        last_timestamp_ns: i64,
    ) -> anyhow::Result<()> {
```

并且在其内部，当 `tx.send(ch_msg).await` 成功时执行位点提交：

```rust
        if tx.send(ch_msg).await.is_ok() {
            tracing::info!(
                "WuKongIM: offline batch sent (silent={}), updating sync state: channel={}:{} seq={}",
                silent,
                channel_id,
                channel_type,
                last_seq
            );
            self.update_sync_state(
                channel_id,
                channel_type,
                last_seq,
                last_timestamp_ns,
            )
            .await?;
        }
```

---

## 质量验证与校验

- [ ] **Step 4：运行 Clippy 静态检查**

执行以下命令，确保没有任何 Clippy 警告或编译报错：
`cargo clippy --all-targets -- -D warnings`

- [ ] **Step 5：运行单元与集成测试**

执行以下命令验证所有的 159+ 测试集完全通过：
`cargo test`

- [ ] **Step 6：检查 Git 状态与格式**

确认没有带入任何无关的全局代码重排改动：
`git diff --stat`
预期修改仅包含两个功能性修改的文件。
