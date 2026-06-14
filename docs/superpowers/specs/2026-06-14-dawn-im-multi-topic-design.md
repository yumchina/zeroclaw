# DawnIM 多话题（Multi-Topic）映射 Thread 设计

> 日期：2026-06-14
> 状态：✅ 设计已与维护方确认（方案 ζ 修订版），待 implementation plan
> 关联：替代 [migration-tracking-TBD.md](../../maintainers/migration-tracking-TBD.md) 中 PR #45 的迁移工作

## 1. 背景与问题

DawnIM 协议层在 `RecvNotificationParams` 中携带逻辑 `topic` 字段，表示同一用户↔同一 bot 之间存在的多条平行"话题线"。每条话题线在用户视角应该有独立的：

- 会话历史（conversation history）
- 记忆（memory）
- 工具调用上下文

0.8.0 当前的 `dawn_im` 模块尚未解析 `topic` —— 所有话题的消息混在同一个 session 里，污染上下文。

### 上游 yumchina PR #45 的方案

yumchina master 的 PR #45 已实现 "topic → `ChannelMessage.thread_ts` 映射" 思路，但：
1. 写在已不存在的旧 `zeroclaw-channel-wukongim` crate 上
2. 包含一个无价值的 `setting: Option<u32>` → `SettingFlags` 对象重构
3. 没把 topic 暴露给工具栈

本设计基于 0.8.0 的 `dawn_im` 模块**重新设计**，不是简单移植 PR #45。

## 2. 设计目标

- DawnIM 多 topic 自动隔离会话历史与记忆（对用户透明）
- 0 配置 — 配 `[channels.dawnim.<alias>]` 后自动生效
- 100% 向下兼容：旧消息 `topic` 为 `"0"` / `""` / 缺失时行为完全不变
- **topic 暴露给工具栈**，为未来"按 topic 类型路由"做信号准备

## 3. 非目标

- **不**做 `SettingFlags` 对象重构 — 继续用 `setting: Option<u32>`，topic flag 用 `Some(8u32)` 表示
- **不**实现 per-topic-ID 的配置覆盖（错向设计）
- **不**实现 topic 分类器 / topic 类型 schema —— 那是独立后续工作
- **不**改 task 消息路径（`SendKind::TaskSubmit` / `TaskQuery`）—— task 发给外部 Agent UID，不需要 topic 上下文
- **不**改通用 orchestrator / `zeroclaw-api::Channel` trait

## 4. 核心机制

### 4.1 入站：协议层 topic → ChannelMessage.thread_ts

```
RecvNotificationParams.topic (DawnIM 协议字段)
    ↓  filter "0" / "" → None
ChannelMessage.thread_ts
    ↓ orchestrator
conversation_history_key()  生成 "dawnim_<reply_target>_<thread_ts>_<sender>"
    ↓
独立的 conversation history + memory namespace
```

`thread_ts` 是 ZeroClaw 既有的"会话内子线程"机制（Slack/Discord/email 早就在用）。`conversation_history_key()` 已经把 `thread_ts` 编进 session key，故 topic → thread_ts 映射后自动获得独立 session。

### 4.2 出站：SendMessage.thread_ts → SendParams.topic + setting bit

```
SendMessage.thread_ts (Some/None)
    ↓ filter
SendParams.topic       = Some(thread_ts) 或 None
SendParams.setting     = Some(8u32) 当 topic 存在；else None
                          (bit-3 置位通知 DawnIM 服务端 topic 字段有效)
```

仅在 `SendKind::Text` 分支映射；`SendKind::TaskSubmit` / `TaskQuery` 路径不动（task 发给外部 Agent UID，不与用户 topic 关联）。

### 4.3 工具感知：ChannelOrigin.topic

`ChannelOrigin` task-local（zeroclaw-api 中定义，0.8.0 T2 引入）扩展新字段 `topic: Option<String>`。orchestrator 在 `process_channel_message_body` 构造 `ChannelOrigin` 时填充 `topic = msg.thread_ts.clone()`。

工具（包括 `CreateTaskTool` / `QueryTaskTool` / 未来的 topic-aware tools）可以通过 `CHANNEL_ORIGIN.try_with(|o| o.topic.clone())` 读取当前 turn 的 topic。

**本期不在 task 路径用到 topic**，但暴露字段让未来的 topic-aware logic 可以独立加上。

## 5. 数据类型变更

### 5.1 `zeroclaw-api/src/channel.rs`

`ChannelOrigin` struct 新增字段：

```rust
#[derive(Clone, Default, Debug)]
pub struct ChannelOrigin {
    pub from_uid: String,
    pub channel_ref: String,
    pub reply_target: String,
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
}
```

### 5.2 `zeroclaw-channels/src/dawn_im/connection.rs`

`RecvNotificationParams` + `SyncMessage` 各新增 `topic: Option<String>` 字段：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecvNotificationParams {
    // ... existing fields ...
    pub timestamp: i64,
    /// DawnIM logical topic identifier. `None` / `Some("")` / `Some("0")`
    /// all mean "no topic" (default thread). Any other value is treated
    /// as an isolated topic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncMessage {
    // ... existing fields ...
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}
```

**不**改 `SendParams.setting: Option<u32>` 类型 — 继续用 u32，topic flag 用 `Some(8u32)` 表示（bit 3 置位）。

### 5.3 `zeroclaw-channels/src/dawn_im/channel.rs`

#### 入站映射 helper（私有 fn）

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

#### Inbound: `process_inbound_message`

在函数顶部计算一次 `topic_thread`：

```rust
let topic_thread = topic_to_thread(params.topic.as_deref());
```

然后在所有 ChannelMessage 构造点（**2 处**：CMD path `la_init_helloworld`、main message path），把现有的 `thread_ts: None` 改为 `thread_ts: topic_thread.clone()`。

#### Outbound: `Channel::send` `SendKind::Text` 分支

在该分支构造 `SendParams` 之前：

```rust
let topic_out = topic_to_thread(message.thread_ts.as_deref());
let setting_out: Option<u32> = topic_out.as_ref().map(|_| 8u32);
```

把现有的：

```rust
let params = SendParams {
    // ...
    setting: None,
    // ...
    topic: None,
};
```

改为：

```rust
let params = SendParams {
    // ...
    setting: setting_out,
    // ...
    topic: topic_out,
};
```

**不**改 `SendKind::TaskSubmit` / `TaskQuery` 分支 — 这些消息发给外部 Agent UID，不携带用户 topic 上下文。

#### Offline batch path: `send_offline_batch_as_single_message`

当前实现把同一 (channel_id, channel_type) 下的多条离线消息合并成**单条** ChannelMessage（line ~1020）。多 topic 引入后，合并需要按 topic 拆分 —— 否则同一 batch 内不同 topic 的消息会被混到一起污染 session。

**决策**：以 (channel_id, channel_type, topic) 为分组键拆分批次，每组生成独立 ChannelMessage。

伪码：

```rust
// process_offline_batch:
// 现状：sorted_messages → send_offline_batch_as_single_message(全部, ...)
// 变更：按 topic 分组后逐组发送
let mut by_topic: HashMap<Option<String>, Vec<...>> = HashMap::new();
for m in sorted_messages {
    let topic = topic_to_thread(m.topic.as_deref());
    by_topic.entry(topic).or_default().push(m);
}
for (topic, group) in by_topic {
    self.send_offline_batch_as_single_message_for_topic(group, topic, silent, tx).await?;
}
```

`send_offline_batch_as_single_message_for_topic` 在原函数基础上加 `topic_thread: Option<String>` 参数，最终 ChannelMessage 构造时填 `thread_ts: topic_thread`。

### 5.4 `zeroclaw-channels/src/orchestrator/mod.rs`

在 `process_channel_message_body` 构造 `channel_origin` 处加 `topic` 字段：

```rust
let channel_origin = zeroclaw_api::channel::ChannelOrigin {
    from_uid: /* unchanged */,
    reply_target: msg.reply_target.clone(),
    channel_ref: /* unchanged */,
    topic: msg.thread_ts.clone(),  // ← 新增
};
```

一行新增。

## 6. 数据流时序

```
① 用户在 DawnIM 同一对话窗口创建 topic="db_lock"
    ↓
② DawnIM 服务端发 RecvNotificationParams { topic: Some("db_lock"), ... }
    ↓
③ DawnIMChannel::process_inbound_message
    let topic_thread = topic_to_thread(params.topic.as_deref()); // Some("db_lock")
    ChannelMessage { thread_ts: Some("db_lock"), ... }
    ↓
④ orchestrator::process_channel_message_body
    history_key = conversation_history_key(&msg)
                = "dawnim_1:u_alice_db_lock_u_alice"  ← thread_ts 已嵌入
    let channel_origin = ChannelOrigin { topic: Some("db_lock"), ... }
    CHANNEL_ORIGIN.scope(channel_origin, run_tool_call_loop(...))
    ↓
⑤ Agent loop 加载 history_key 对应的独立 conversation history + memory
    （与无 topic 的 "dawnim_1:u_alice_u_alice" 完全不同）
    ↓
⑥ LLM 回复 → SendMessage { thread_ts: Some("db_lock"), ... }
    （SendMessage::reply_to 自动继承 inbound thread_ts）
    ↓
⑦ DawnIMChannel::send (SendKind::Text):
    topic_out = Some("db_lock")
    setting_out = Some(8u32)
    SendParams { topic: Some("db_lock"), setting: Some(8), ... }
    → DawnIM 服务端识别 bit-3 置位，按 topic 投递
    ↓
⑧ 用户在 db_lock topic 窗口收到回复
```

## 7. 错误模式

| 场景 | 行为 |
|------|------|
| `params.topic == None`（旧客户端） | `topic_thread = None`，走 legacy 单线程 session（向下兼容） |
| `params.topic == Some("0")` 或 `Some("")` | 同上 — sentinel 值过滤为 None |
| 同一 offline batch 包含多个 topic | 拆分发送，每个 topic 独立 ChannelMessage |
| 出站 `SendMessage.thread_ts == None` | `topic: None, setting: None`（不置位）— 不变 |
| 出站 `SendMessage.thread_ts == Some(...)` | `topic: Some(...), setting: Some(8u32)` — bit-3 置位 |

## 8. 改动清单

按 crate 分组。

### 8.1 `zeroclaw-api`

| 文件 | 操作 | 内容 |
|------|------|------|
| `src/channel.rs` | 修改 | `ChannelOrigin` struct 加 `topic: Option<String>` 字段 + 英文 doc comment |

### 8.2 `zeroclaw-channels`

| 文件 | 操作 | 内容 |
|------|------|------|
| `src/dawn_im/connection.rs` | 修改 | `RecvNotificationParams` 加 `topic: Option<String>` 字段；`SyncMessage` 加同字段 |
| `src/dawn_im/channel.rs` | 修改 | (1) 新增 `topic_to_thread` 私有 helper；(2) `process_inbound_message` 顶部计算 `topic_thread`，所有 `ChannelMessage` 构造点的 `thread_ts: None` 改为 `thread_ts: topic_thread.clone()`；(3) `Channel::send` `SendKind::Text` 分支映射 `message.thread_ts` → `SendParams.topic + setting:Some(8u32)`；(4) `process_offline_batch` 按 topic 分组后调 `send_offline_batch_as_single_message`（后者加 `topic_thread` 参数） |
| `src/orchestrator/mod.rs` | 修改 | `process_channel_message_body` 构造 `ChannelOrigin` 时加 `topic: msg.thread_ts.clone()` 字段 |

### 8.3 新增文件

无。设计文档之外不新增任何代码文件。

## 9. 行数估算

| 净变化 | 行数 |
|--------|------|
| `ChannelOrigin.topic` 字段 + doc | ~10 行 |
| `RecvNotificationParams.topic` / `SyncMessage.topic` 字段 | ~10 行 |
| `topic_to_thread` helper | ~8 行 |
| 入站映射（替换 `thread_ts: None`） | ~5 行净（替换） |
| 出站映射（替换 `setting: None, topic: None`） | ~10 行净（含 let bindings） |
| offline batch 按 topic 分组 | ~30 行 |
| orchestrator `ChannelOrigin.topic` 填充 | ~1 行 |
| 测试 | ~80 行 |
| **合计** | **~155 行** |

## 10. 兼容性

### 10.1 用户配置兼容性

**无 config 变更**。所有现有 `[channels.dawnim.<alias>]` 配置零修改即可生效新行为。

### 10.2 历史会话兼容性

DawnIM 服务端发给旧客户端的消息不带 topic，本设计 `topic_to_thread` 会把所有非有效 topic 归一到 `None`，`conversation_history_key` 生成的 session key 与改造前完全相同。**历史 conversation / memory 100% 复用**。

### 10.3 wire 协议兼容性

- 入站：协议加字段是 serde-optional，缺失 → `None`，与服务端不发该字段时行为一致
- 出站：服务端早就识别 `topic` + `setting` bit-3（这是 DawnIM 设计的一部分），无 wire 兼容性风险
- `setting` 继续是 `Option<u32>` —— 不引入 SettingFlags 对象，避开服务端是否接受 object 形式 setting 的未知

### 10.4 Channel 实现者兼容性

`ChannelOrigin.topic` 是 task-local 字段加一项 — 任何现有 channel 实现不感知（默认 None），新行为对它们透明。

## 11. AGENTS.md 单一事实源审视

| 数据项 | 事实源 | 各处如何拿 |
|--------|--------|-----------|
| 当前 turn 的 topic | inbound `ChannelMessage.thread_ts`（由 DawnIM 协议 `topic` 字段派生） | orchestrator 读 `msg.thread_ts` 时同时填进 `ChannelOrigin.topic`；工具通过 `CHANNEL_ORIGIN.try_with(|o| o.topic.clone())` 读 |
| 历史 session key | `conversation_history_key(&msg)` 函数，纯函数从 `msg.thread_ts` + `msg.reply_target` + `msg.sender` 计算 | 无缓存，调用即算 |
| topic 过滤规则（"0" / "" → None） | `topic_to_thread()` helper 单点定义 | 入站映射与离线批次拆分都走这一个 helper |

无重复 state。

## 12. 验证计划

- `cargo check --workspace --all-targets`：默认 features + channel-dawnIM 全过
- `cargo test -p zeroclaw-api`：新增 `ChannelOrigin.topic` 默认值 + scope 测试
- `cargo test -p zeroclaw-channels --features channel-dawnIM`：
  - `topic_to_thread` 单元测试（None / "" / "0" / Some("db_lock") 四种 case）
  - `process_inbound_message`：构造带 topic 的 RecvNotificationParams，断言生成的 ChannelMessage.thread_ts 正确
  - `Channel::send` SendKind::Text：构造带 thread_ts 的 SendMessage，断言（用 mock WS）SendParams.topic + setting 正确
  - offline batch 按 topic 拆分：构造跨多 topic 的 batch，断言生成多条 ChannelMessage

## 13. 风险与缓解

| 风险 | 缓解 |
|------|------|
| DawnIM 服务端发出的 `topic` 字段格式与假设不同（除 `"0"`/`""` 外还有其他 sentinel） | `topic_to_thread` 集中处理，未来发现新 sentinel 一处修改 |
| 历史 conversation history 因 thread_ts 计算改变而丢失 | 不会 — 历史消息 `params.topic = None` 时 thread_ts 仍为 None，session key 不变 |
| offline batch 拆分后投递顺序混乱 | 按 topic 分组保留组内时间序；不同 topic 之间无序无关紧要（不同 topic 是独立 session） |
| `setting: Some(8u32)` 与服务端协议不匹配 | 与 PR #45 的 commit 1（同样用 u32）一致，且原 PR 已在 yumchina 生产验证 |
| 未来要做 topic 类型路由（用户提到的"分析型/编码型"）时本设计是否够用 | 够 — 任何未来的分类器都需要先拿到 topic ID，`ChannelOrigin.topic` 正是这个上游信号；分类器 + 类型 config 是独立后续工作，不需本期预设接口 |
