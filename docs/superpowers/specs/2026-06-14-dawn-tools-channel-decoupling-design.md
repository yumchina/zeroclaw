# dawn-tools 与 channel 解耦设计

> 日期：2026-06-14
> 状态：✅ 设计已与维护方确认，待 implementation plan
> 关联：[migration-tracking-TBD.md](../../maintainers/migration-tracking-TBD.md) #39+#46 后续优化

## 1. 背景与问题

PR #39 + #46 已迁移 dawn task 工具到 0.8.0（见
`crates/dawn-tools/src/task.rs` 和 `crates/zeroclaw-channels/src/orchestrator/mod.rs`
的当前实现）。迁移采用 "全局 mpsc bridge + listener" 模式：dawn-tools 通过
`CHANNEL_BRIDGE` 静态变量推 `TaskMessage`，orchestrator 在 `start_channels`
中 spawn 一个 listener 转发给 `DawnIMChannel::send_status_message`。

这个实现在 0.8.0 架构下存在三个根本问题：

1. **错误的 crate 依赖方向**。`zeroclaw-channels`（通道实现）反向依赖
   `dawn-tools`（工具实现）。工具应该是叶子或近叶子节点，channel 不该
   编译时需要知道任何具体工具的存在。

2. **硬编码到 DawnIM**。listener 内部 `wk.send_status_message(...)` 硬绑定
   DawnIMChannel。这与 0.8.0 引入的两个新概念冲突：
   - **master channel**：`[channels].master_channel` 让用户跨多个 channel 实例
     合并身份，"哪个 channel 是主"是配置驱动而非硬编码
   - **多 dawnim 实例**：`config.channels.dawnim: HashMap<alias, ...>`
     现行 listener 虽按 alias 路由，但仍局限于"DawnIM 这类 channel"

3. **dawn-tools 作为工具家族被绑死**。维护方明确表达：dawn_task 的语义是
   "通过某个 channel 把任务交给远程 executor，结果原路返回"。任何 channel
   都应能承担这个角色（如 wechat 接 KFC 点餐 Agent，dawnim 接璇玑文档提取）。
   现在的设计排除了这种可能。

## 2. 设计目标

- 消除 `zeroclaw-channels → dawn-tools` 的编译时依赖
- 任意 channel 通过实现 `Channel::send` 的 task 分支即可参与 task executor
  能力，无需新 trait、无需 channel-specific flag
- 配置驱动：每个 `[dawn_task.<n>]` 显式声明 "通过哪个 channel 实例发任务"
- 错误信息可读（启动期报告配置 typo，运行期 channel 不支持 task 时返
  Err 带 channel name）
- 与 0.8.0 已有的 `PerToolChannelHandle` 模式 1:1 对齐

## 3. 非目标

- 不引入新 trait（不创建 `TaskChannel` 或类似抽象）
- 不支持单 task 绑定多 channel 的 failover / 并发 / load-balance（YAGNI，
  绑定单一 channel 即可）
- 不实现"自动回信路由到 master_channel"——回信走原 channel 是 channel 协议
  本身的属性，不是路由决策
- 不动 SendMessage 现有字段或语义（content / recipient / subject / thread_ts
  / attachments / in_reply_to / cancellation_token 不变）

## 4. 架构概览

```
zeroclaw-api                  ← 新增 SendKind, ChannelOrigin, CHANNEL_ORIGIN
   ↑                ↑
zeroclaw-channels   dawn-tools
   ├ DawnIMChannel: 在 Channel::send 内部 match kind 分支
   │   ├ SendKind::Text → 现有 type=1 路径
   │   └ SendKind::TaskSubmit/Query → 新的 type=2000 CMD 路径
   ├ orchestrator::register_channels_for_tools
   │   填 task_channel_handle: PerToolChannelHandle
   └ orchestrator::process_channel_message_body
       CHANNEL_ORIGIN.scope(...) 包裹 run_tool_call_loop

   └ CreateTaskTool / QueryTaskTool
       字段: Arc<Config> + PerToolChannelHandle
       execute: config 查 executor → handle 查 Arc<dyn Channel>
                → 构造 SendMessage{kind: SendKind::TaskSubmit{..}}
                → channel.send(&msg).await
```

**关键不变量**：dawn-tools 只依赖 `zeroclaw-api`（Channel trait + SendMessage）
和 `zeroclaw-config`（DawnTaskExecutors schema），永不依赖任何具体 channel
实现。channels 也不依赖 dawn-tools。

## 5. 数据类型设计

### 5.1 `zeroclaw-api` 新增

```rust
// src/channel.rs

/// 消息类型分类。决定 Channel::send 实现走哪条编码路径。
#[derive(Debug, Clone, Default)]
pub enum SendKind {
    /// 普通用户对话消息（兼容现有 30+ channel 零修改）
    #[default]
    Text,
    /// 提交任务给 channel 对端的外部 executor
    TaskSubmit {
        task_type: u8,
        user_id: String,              // 原始用户 ID（executor 用以寻址回信）
        user_text: String,
        params: serde_json::Value,
    },
    /// 查询任务状态
    TaskQuery {
        task_type: u8,
        user_id: String,
        task_id: String,
    },
}

/// SendMessage 扩展 kind 字段（其他字段不变）
#[derive(Debug, Clone, Default)]
pub struct SendMessage {
    pub content: String,
    pub recipient: String,
    pub subject: Option<String>,
    pub thread_ts: Option<String>,
    pub cancellation_token: Option<CancellationToken>,
    pub attachments: Vec<MediaAttachment>,
    pub in_reply_to: Option<String>,
    pub kind: SendKind,               // ← 新增
}

/// 一个 turn 的来源上下文：哪个用户从哪个 channel 实例触发
#[derive(Clone, Default, Debug)]
pub struct ChannelOrigin {
    pub from_uid: String,             // _la_ 后缀已剥
    pub channel_ref: String,          // "<type>.<alias>" e.g. "dawnim.work"
    pub reply_target: String,         // msg.reply_target 原值
}

tokio::task_local! {
    /// orchestrator 在 process_channel_message_body 内 scope 此值；
    /// 工具通过 try_with 读取，知道当前消息出处。
    pub static CHANNEL_ORIGIN: ChannelOrigin;
}
```

`SendMessage` 加 `#[derive(Default)]` 后，所有现有 `SendMessage::new(...)` /
`with_subject` / `reply_to` 等构造点继续工作 — `kind` 自动取
`SendKind::Text`。

### 5.2 `Channel::send` trait 形态不变

`send` 在 0.8.0 是 required method（无 default impl），30+ channel 都已 override。
**本设计不改 trait shape**，每个 channel impl 自行决定如何处理 `kind`：

- **不支持 task 的 channel**（默认情况）：现有 send impl 只处理 Text-shaped 消息；
  收到 task kind 时应主动 bail。提供小 helper 让其一行处理：

```rust
// zeroclaw-api/src/channel.rs (新增 SendMessage 关联函数)
impl SendMessage {
    /// Channels that don't support non-Text kinds can call this at the top
    /// of their `send` impl to reject task / future kinds with a readable error.
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
}
```

  本设计**不强制**现有 30+ channel 调用此 helper —— 现状下 dawn_create_task
  tool 只往 `[dawn_task.<n>].channel` 配置的 channel 发 task kind，
  非 dawnim channel 即使没加 `ensure_text_kind` 也不会意外收到。helper 仅
  对运维或测试场景下"手动构造 task kind 直接调任意 channel"提供防御。

- **支持 task 的 channel**（如 DawnIM）：在 send impl 顶部 match kind 显式分支，
  Text 路径走老逻辑，TaskSubmit / TaskQuery 走新协议。match 写为穷尽（不用 `_`）
  以便后续加 variant 时编译器强制提醒所有支持 channel 同步更新。

### 5.3 `zeroclaw-config::dawn_task` 重构

```rust
// src/dawn_task.rs

/// 单个 task 类型的 executor 配置（"任务由谁/通过哪个管道执行"）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DawnTaskExecutorConfig {
    /// Composite channel key "<type>.<alias>", e.g. "dawnim.work"
    pub channel: String,
    /// Channel-specific addressee:
    /// - dawnim: agent UID, e.g. "1878_xuanji_agent"
    /// - wechat: openid / group_id
    /// - slack: webhook URL or user/channel ID
    pub recipient: String,
    /// Human-readable name for logging and operator UX
    pub name: String,
    /// Description of what this executor does (used in tool prompt)
    pub description: String,
}

/// task type id → executor 的注册表
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DawnTaskExecutors {
    #[serde(flatten)]
    pub executors: HashMap<String, DawnTaskExecutorConfig>,
}

impl DawnTaskExecutors {
    pub fn get_by_type(&self, task_type: u8) -> Option<&DawnTaskExecutorConfig> {
        self.executors.get(&task_type.to_string())
    }
}
```

TOML schema 示例：

```toml
[dawn_task.1]
channel     = "dawnim.work"
recipient   = "1878_xuanji_agent"
name        = "璇玑文档提取"
description = "提取 PDF/Word/PPT/Excel 等文档内容"

[dawn_task.2]
channel     = "wechat.main"
recipient   = "wxid_kfc_ordering_bot"
name        = "KFC 点餐 Agent"
description = "通过微信群下单"
```

### 5.4 `Config` 字段

```rust
// zeroclaw-config/src/schema.rs
pub struct Config {
    // ...
    pub dawn_task: crate::dawn_task::DawnTaskExecutors,
    // ...
}
```

TOML key (`dawn_task`) 不变，类型升级为 `DawnTaskExecutors`。

## 6. 运行时流程

```
① 用户从 dawnim.work 发消息 "帮我提取这个 PDF"
    ↓
② DawnIMChannel::listen → ChannelMessage{channel: "dawnim",
    channel_alias: Some("work"), sender: "u_alice_la_1780...",
    reply_target: "1:u_alice", ...}
    ↓
③ orchestrator::process_channel_message_body
    let origin = ChannelOrigin {
        from_uid: "u_alice",            // _la_<bot_uid> 后缀剥离
        channel_ref: "dawnim.work",
        reply_target: "1:u_alice",
    };
    CHANNEL_ORIGIN.scope(origin, run_tool_call_loop(...))
    ↓
④ Agent loop → LLM 决定调用 dawn_create_task(type=1, user_text, params)
    ↓
⑤ CreateTaskTool::execute(args):
    a. task_type = args["type"] as u8
    b. let exec = self.config.dawn_task.get_by_type(task_type)?
       // exec.channel = "dawnim.work", exec.recipient = "1878_xuanji_agent"
    c. let channel: Arc<dyn Channel> = {
           let map = self.channel_handle.read();
           map.get(&exec.channel).cloned()
               .ok_or("channel '{}' 未注册", exec.channel)?
       };
    d. let origin = CHANNEL_ORIGIN.try_with(|o| o.clone()).unwrap_or_default();
    e. let msg = SendMessage {
           recipient: exec.recipient.clone(),
           kind: SendKind::TaskSubmit {
               task_type,
               user_id: origin.from_uid,
               user_text: args["user_text"].as_str()?.into(),
               params: args["params"].clone(),
           },
           ..Default::default()
       };
       channel.send(&msg).await?;
    f. Ok(ToolResult{success: true, output: "已提交...", ..})
    ↓
⑥ DawnIMChannel::send(&msg) — match kind:
    SendKind::TaskSubmit { task_type, user_id, user_text, params } =>
        let payload = json!({
            "type": 2000,
            "cmd": "dawn.create_task",
            "param": {
                "type": task_type,
                "user_id": user_id,
                "user_text": user_text,
                "params": params,
                "reply_to": self.uid,    // ← DawnIM 自动填自己的 UID
            }
        });
        let b64 = base64::encode(payload.to_string());
        self.send_rpc("send", SendParams {
            channel_id: msg.recipient.clone(),    // executor agent UID
            channel_type: 1,
            payload: Value::String(b64),
            from_uid: Some(self.uid.clone()),
            ..
        }).await?;
    ↓
═══════════ 异步等待远程 executor 处理 ═══════════
    ↓
⑦ 璇玑 Agent 处理完成 → 回传 CMD type=2000 cmd="...task_complete"
   → DawnIMChannel::listen 收到 → ChannelMessage 注入 orchestrator
   → 路由回 u_alice 的 session 继续会话
```

## 7. 错误模式

| 场景 | 错误来源 | 错误信息 / 处理 |
|------|---------|----------------|
| `task_type` 未配置 | `dawn_task.get_by_type(N)` -> None | 工具返 Err "未配置 type=N 的 dawn task" |
| 配置的 channel 在 handle map 不存在 | `map.get("dawnim.work")` -> None | 工具返 Err "channel 'dawnim.work' 未启动或未注册" |
| Channel 不支持 task kind 但工具仍尝试发送 | 由配置 gate 防止（dawn_create_task 只发到 `[dawn_task.<n>].channel` 列出的 channel）。万一漏配，该 channel 的 send impl 行为依赖其自身（可选 `ensure_text_kind` helper 防御性 bail） | 启动期 WARN（见下行）+ channel impl 显式 bail |
| CLI / 非 channel 上下文调用 | `CHANNEL_ORIGIN.try_with` -> Default | user_id 为空；executor 收到空 user_id 自行决策。工具不强校验（YAGNI；channel-binding 工具被 CLI 场景使用本就罕见） |
| 启动期 config 配的 channel 不存在 | `register_channels_for_tools` 后扫描比对 | WARN：`dawn_task.<n> 配置的 channel '<x>' 未注册或未启用，相关 task 将不可用` |

## 8. 改动清单

按 crate 分组。

### 8.1 `zeroclaw-api`

| 文件 | 操作 | 内容 |
|------|------|------|
| `src/channel.rs` | 修改 | 新增 `SendKind` 枚举（含 `#[default]` Text）；`SendMessage` 加 `kind` 字段 + `#[derive(Default)]`；新增 `SendMessage::ensure_text_kind(channel_name)` helper；新增 `ChannelOrigin` + `CHANNEL_ORIGIN` task-local。`Channel::send` trait 形态不变（仍是 required，无 default）。 |

### 8.2 `zeroclaw-config`

| 文件 | 操作 | 内容 |
|------|------|------|
| `src/dawn_task.rs` | 修改 | 类型 `DawnTasks` → `DawnTaskExecutors`；字段 `tasks` → `executors`；类型 `DawnTaskConfig` → `DawnTaskExecutorConfig`；加 `channel: String` 字段，旧字段 `uid` 改名 `recipient`；更新 tests |
| `src/schema.rs` | 修改 | 3 处 Default 站点：`crate::dawn_task::DawnTaskExecutors::default()` |

### 8.3 `zeroclaw-channels`

| 文件 | 操作 | 内容 |
|------|------|------|
| `Cargo.toml` | 修改 | **删除** `dawn-tools.workspace = true`（解耦关键） |
| `src/dawn_im/channel.rs` | 修改 | 删 `send_status_message` 方法；`Channel::send` impl 开头 match kind，Text 走老 type=1 路径，TaskSubmit/TaskQuery 走新 type=2000 CMD 路径；`reply_to` 自动填 `self.uid` |
| `src/orchestrator/mod.rs` | 修改 | 删 `CollectedChannels.dawn_im_channels`；删 `start_channels` 中桥接 listener spawn（~100 行）；`register_channels_for_tools` 加 `task_channel_handle: &Option<PerToolChannelHandle>` 入参；`process_channel_message_body` 中 `dawn_tools::TaskContext` 替换为 `zeroclaw_api::channel::CHANNEL_ORIGIN`，构造 `ChannelOrigin` 注入 |

### 8.4 `dawn-tools`

| 文件 | 操作 | 内容 |
|------|------|------|
| `Cargo.toml` | 修改 | 删 `parking_lot`（bridge 没了不再需要） |
| `src/task.rs` | 重写 | 删 `TaskMessage` / `CHANNEL_BRIDGE` / `set_channel_bridge` / `TaskContext` / `TASK_CONTEXT`；`CreateTaskTool` / `QueryTaskTool` 字段改为 `(config: Arc<Config>, channels: PerToolChannelHandle)`；`execute` 按 §6 流程；新 tests mock `Arc<dyn Channel>` 验证 SendMessage 构造正确性 |
| `src/lib.rs` | 修改 | re-export 删除 bridge 相关项；保留 `CreateTaskTool` / `QueryTaskTool` |

### 8.5 `zeroclaw-runtime`

| 文件 | 操作 | 内容 |
|------|------|------|
| `src/tools/mod.rs` | 修改 | `AllToolsResult` 加 `task_channel_handle: PerToolChannelHandle`；`all_tools_with_runtime` 创建 handle + 注册 `CreateTaskTool::new(cfg_arc, task_channel_handle.clone())` 和 `QueryTaskTool::new(...)`；注册条件 `!root_config.dawn_task.executors.is_empty()` |
| `src/daemon/mod.rs` 或 agent 入口 | 修改 | 从 `AllToolsResult` 接出 `task_channel_handle`，传给 `register_channels_for_tools`。**`register_channels_for_tools` 返回后**调用 `validate_dawn_task_executors(config, &task_channel_handle.read())` 做软校验，对每个 `executor.channel` 配的 channel ref 不在 handle map 中则 WARN（不报错，不阻止启动） |

### 8.6 调用方更新（机械改动）

`register_channels_for_tools` 所有调用点（grep 该函数名）补充新参数。

## 9. 行数估算

| 净变化 | 行数 |
|--------|------|
| 删除（bridge infra + send_status_message + listener） | ~210 行 |
| 新增（SendKind + ChannelOrigin + executor schema + handle wiring + match kind impl） | ~170 行 |
| **净减** | **~40 行** + 1 个 crate 依赖去除 + 1 个全局静态消除 |

## 10. 兼容性 / 迁移

### 10.1 用户配置迁移

旧 schema：

```toml
[dawn_task.1]
uid         = "1878_xuanji_agent"     # ← 改名为 recipient
name        = "璇玑"
description = "..."
```

新 schema：

```toml
[dawn_task.1]
channel     = "dawnim.work"           # ← 新增必填
recipient   = "1878_xuanji_agent"     # ← uid 改名
name        = "璇玑"
description = "..."
```

启动期 config 加载时缺 `channel` 字段直接报错指明迁移路径（已通过 serde
required field 实现）。

### 10.2 代码迁移

- `SendMessage::new(...)` / `::with_subject(...)` / `::reply_to(...)` 等所有
  现有构造点 **零修改** —— `kind` 走 `Default::default() == SendKind::Text`
- `Channel` 实现者 **零修改**（除非自己想接 task）—— Channel::send trait
  shape 不变，现有 send impl 处理 Text-shaped 消息的逻辑不变。配置 gate
  保证非 dawnim channel 不会意外收到 task kind。

### 10.3 新 channel 接 task 能力

只需在该 channel 的 `Channel::send` impl 里多一个 match 分支：

```rust
async fn send(&self, message: &SendMessage) -> Result<()> {
    match &message.kind {
        SendKind::Text => self.send_text(...).await,
        SendKind::TaskSubmit { task_type, user_id, user_text, params } => {
            // channel-native 编码 + 投递
        }
        SendKind::TaskQuery { .. } => { /* 类似 */ }
    }
}
```

不需要 impl 任何额外 trait，不需要修改其他 crate。

## 11. AGENTS.md 单一事实源审视

| 数据项 | 事实源 | 各处如何拿 |
|--------|--------|-----------|
| Channel 实例 | orchestrator 构造的 `Arc<dyn Channel>` | 唯一 Arc 同时填进 ask_user / reaction / poll / escalate / **task** 等所有 PerToolChannelHandle。引用计数共享，零数据拷贝 |
| Task type → executor 配置 | `Config.dawn_task.executors` | 工具持 `Arc<Config>` 快照（与 DawnS3Tool 等一致），`/admin/reload` 重建 tools registry 时自动刷新 |
| `la_id`（DawnIM bot UID） | `DawnIMChannel.uid` 字段 | DawnIM::send 内部 `self.uid` 自取，**不暴露给工具** —— 解决了之前迁移设计中"工具需要查 config 拿 la_id"的反 DRY 痛点 |
| Originating user | `CHANNEL_ORIGIN.from_uid` | orchestrator scope 注入，工具 try_with 读 |

无重复 state。

## 12. 验证计划

- `cargo check --workspace --all-targets` 全过（默认 features + channel-dawnIM
  + dawn-tools）
- `cargo test -p zeroclaw-api`：新增 SendKind 序列化 / Default 测试
- `cargo test -p zeroclaw-config dawn_task`：DawnTaskExecutors schema 测试
- `cargo test -p dawn-tools task`：用 mock `Arc<dyn Channel>` 验证
  CreateTaskTool / QueryTaskTool 构造和投递正确的 SendMessage
- `cargo test -p zeroclaw-channels dawn_im`：DawnIMChannel::send 的 match
  kind 分支编码正确性（Text 路径回归 + TaskSubmit/TaskQuery 编码正确）
- 启动期软校验：配置中 channel typo 时打印 WARN，相关 task 提交时返
  可读 Err

## 13. 风险与缓解

| 风险 | 缓解 |
|------|------|
| `SendMessage.kind` 演化为 god-object（未来再加 ApprovalCard / Heartbeat / VoiceCall 等 variant） | 短期 YAGNI；如真出现一类完全独立的消息族，再讨论拆 trait。当前 task 与 Text 共用 send 通路是合理的（都走"对 channel 发出去"） |
| 工具持 `Arc<Config>` 快照与 reload 时机错位 | 与 DawnS3Tool / DawnWebSearchTool 等一致；`/admin/reload` 重建 tools registry 已是既定路径 |
| Channel impl 漏掉新 SendKind variant 编译警告 | 支持 task 的 channel match 写为穷尽（不用 `_`），编译器强制覆盖所有 variant |
| 配置 channel 字段 typo 导致 task 长期失效 | 启动期 WARN + 运行期 Err with channel name —— 双重保险 |
| 未审计的 channel 收到 task kind 时行为未定义（按 Text 处理 → 静默丢/出错） | (1) 配置 gate：dawn_create_task 只往 `[dawn_task.<n>].channel` 配置的 channel 发，不会意外打到任意 channel；(2) `SendMessage::ensure_text_kind(channel_name)` helper 供谨慎的 channel impl 一行防御；(3) 启动期软校验 + 运行期 Err 双保险 |
