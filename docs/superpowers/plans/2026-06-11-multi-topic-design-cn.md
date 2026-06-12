# WuKongIM 多话题（Multi-Topic）映射 Thread 重构实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 
在不修改通用编排层 `zeroclaw-channels` 和 `zeroclaw-api` 的前提下，通过在 `zeroclaw-channel-wukongim` 通道插件中将逻辑 **Topic** 映射为 ZeroClaw 现有的物理 **Thread (`thread_ts`)** 机制，无缝对接 Dawn 客户端多会话隔离设计。
将 `"0"` 和 `""` 均规范化为 `None`（默认无话题），确保与历史会话和数据的 100% 向下兼容。

---

## 核心设计与数据流转

```mermaid
graph TD
    subgraph WuKongIM Channel (Inbound)
        in[RecvNotificationParams] --> parse{解析 params.topic}
        parse -->|topic为 '0' 或 ""| thread_none[msg.thread_ts = None]
        parse -->|topic为有效值 'db_lock'| thread_val[msg.thread_ts = Some('db_lock')]
    end
    
    subgraph Orchestrator (No changes required)
        thread_none --> legacy_key[legacy Key: wukongim_replytarget_sender]
        thread_val --> topic_key[isolated Key: wukongim_replytarget_dblock_sender]
        
        legacy_key --> legacy_session[加载默认会话/记忆]
        topic_key --> topic_session[加载 'db_lock' 隔离会话/记忆]
    end

    subgraph WuKongIM Channel (Outbound)
        reply[SendMessage] --> send{发送消息}
        send -->|msg.thread_ts 为 Some('db_lock')| set_topic[设置 SendParams.topic = 'db_lock' 且 setting |= 8]
        send -->|msg.thread_ts 为 None| omit_topic[不设置 topic 且不置位 setting]
    end
```

---

## 文件结构

**修改：**
- [zeroclaw-channel-wukongim (protocol)](file:///Users/mengliang/project/zeroclaw/crates/zeroclaw-channel-wukongim/src/connection/protocol.rs) —— 在 `RecvNotificationParams` 协议结构体中追加 `topic` 反序列化。
- [zeroclaw-channel-wukongim (channel)](file:///Users/mengliang/project/zeroclaw/crates/zeroclaw-channel-wukongim/src/channel.rs) —— 
  1. 在接收消息和离线消息批处理时，将有效 topic（过滤 `"0"` 和 `""`）映射到 `ChannelMessage.thread_ts`。
  2. 在发送消息和发送状态更新时，将 `SendMessage.thread_ts` 映射回 `SendParams.topic` 并将 `setting` 进行第 3 位（`1 << 3 = 8`）置位。

---

## Task 1：升级 WuKongIM 通道协议解析 (zeroclaw-channel-wukongim)

**文件：**
- 修改：`crates/zeroclaw-channel-wukongim/src/connection/protocol.rs`
- 修改：`crates/zeroclaw-channel-wukongim/src/channel.rs`

- [ ] **Step 1：在 `RecvNotificationParams` 中反序列化 `topic`**
  编辑 `protocol.rs`，使 WuKongIM 接收到的通知数据结构能解析出 `topic`：
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct RecvNotificationParams {
      // ... 现有字段 ...
      pub timestamp: i64,
      #[serde(skip_serializing_if = "Option::is_none")]
      pub topic: Option<String>,
  }
  ```

- [ ] **Step 2：在入站消息转换中将 Topic 映射为 `thread_ts`**
  编辑 `channel.rs`，重构 `process_inbound_message` 和 `send_offline_batch_as_single_message`，通过过滤 `"0"` 和 `""` 将逻辑话题映射为 `thread_ts`：
  ```rust
  let topic_thread = params.topic.as_deref()
      .filter(|&t| !t.is_empty() && t != "0")
      .map(|s| s.to_string());

  let ch_msg = ChannelMessage {
      id: params.message_id,
      sender: target_id.clone(),
      reply_target: format!("{}:{}", params.channel_type, target_id),
      content,
      channel: "wukongim".to_string(),
      timestamp: params.timestamp.max(0) as u64,
      thread_ts: topic_thread, // 映射到 thread_ts 字段
      interruption_scope_id: None,
      attachments: vec![],
  };
  ```

- [ ] **Step 3：在发送消息 `send` 和 `send_status_update` 中映射并置位 Topic**
  在 `send` 和 `send_status_update` 中，判断 `message.thread_ts` 是否存在：
  ```rust
  let mut setting = None;
  let topic = message.thread_ts.as_ref().filter(|t| !t.is_empty() && *t != "0").map(|s| s.to_string());
  if topic.is_some() {
      setting = Some(8); // Bit 3置位 (1 << 3)
  }

  let params = SendParams {
      from_uid: Some(self.uid.clone()),
      client_msg_no: Uuid::new_v4().to_string(),
      channel_id,
      channel_type,
      payload: serde_json::Value::String(payload_b64),
      header: None,
      setting, // 置位 setting
      msg_key: None,
      expire: None,
      stream_no: None,
      topic, // 传回 topic
  };
  ```

---

## 质量验证与校验

- [ ] **Step 1：执行代码编译和 Clippy 静态扫描**
  在本地运行如下指令进行完整质量扫描，确保无报错：
  `cargo clippy --all-targets -- -D warnings`

- [ ] **Step 2：运行单元与集成测试**
  在本地工作目录中执行全部自动化组件测试确保 100% Pass：
  `cargo test`
