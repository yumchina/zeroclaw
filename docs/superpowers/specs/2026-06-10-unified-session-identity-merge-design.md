# 统一会话：跨端身份合并设计

- 创建：2026-06-10
- 更新：2026-06-11（身份模型改为 master channel + 配对式 /bind）
- 状态：待评审
- 关联分支：0.8.0

## 1. 背景

ZeroClaw 当前的会话历史按 `session_key` 隔离。`session_key` 由
`conversation_history_key(msg)`（`crates/zeroclaw-channels/src/orchestrator/mod.rs:472`）
这个纯函数计算，构成大致为：

```
{channel}.{alias}_{reply_target}_{sender}              // 普通
{channel}.{alias}_{reply_target}_{thread_ts}_{sender}  // 线程/论坛
```

这个 key 同时是三处的标识：

1. 内存中 conversation history cache 的 HashMap key；
2. SQLite session 后端（`{workspace}/sessions/sessions.db`）`sessions` 表与
   `session_metadata` 表的 `session_key` **列**（不是表名）；
3. memory backend 的 `session_id` 过滤字段。

因为 key 以 `channel` 开头，不同渠道天然算出不同 key——这是 by-design 的渠道隔离。

`ChatMessage` 仅有 `{role, content}` 两个字段（`crates/zeroclaw-api/src/model_provider.rs:22`），
单条历史消息**不携带 sender id 或来源渠道**。

## 2. 目标

同一个真人在多个**已配置子渠道**（dawnim / lark / wecom / qq / wechat 等）的
**1:1 私聊**会话历史，合并到一个统一会话，使 agent 跨端看到连贯上下文。

## 3. 非目标（明确排除）

- **不**做群聊合并：仅 1:1 私聊。
- **不**迁移存量历史：仅对启用后的新消息生效。
- **不**合并长期 memory：本期只合并会话历史（session 层）。
- **不**做自动身份识别：跨端身份靠 master channel + 用户自助 `/bind`。
- **不**把统一渠道做成独立 Channel：见 §5。

## 4. 身份模型（核心）

### 4.1 master channel

- 全局**唯一**一个 master channel。master channel 上的用户 id **就是**统一身份 `person_id`。
- 配置方式（主推）：在 `[channels]` 下单值声明

  ```toml
  [channels]
  master_channel = "dawnim.work"   # "<type>.<alias>"，全局唯一
  superusers = ["u_alice", "u_bob"]  # master channel 上的 user id 列表
  ```

  - 备选写法（用户原始倾向）：给每个渠道 config struct 增加 `master = true` flag，
    运行时校验全局仅一个为 true。**主推单值 `master_channel`**：天然唯一、零 struct 改动。
- 是否配置了 `master_channel` 即作为本功能的**隐式总开关**——未配置则 resolver 为 `None`，
  行为与现状 100% 一致，不单设额外开关。

### 4.2 superusers 与 unified_member

- `superusers`：master channel 的 user id 列表（即一批 `person_id`）。
- 首次初始化（首次创建 `identity.db` 或首次启动检测到表为空）时，把 `superusers`
  写入 `unified_member` 表。
- `unified_member` 语义：**启用跨端合并的 master id 白名单**。仅白名单内的 master id
  才参与统一会话；`/bind` 只能绑定到白名单内的 id。

### 4.3 配对式 /bind（建立从渠道映射）

从渠道（非 master）的 `sender` 必须显式绑定到某个 master id 才能并入统一会话。
采用**配对式**绑定，防止冒充（绑定意味着可读对方完整会话历史）：

1. 用户在 **master 渠道**输入 `/bind` → 系统生成一个短期一次性 code（如 6 位），回给用户。
   code 与该用户的 master id 关联，存内存，TTL 数分钟。
2. 用户在 **从渠道**输入 `/bind <code>` → 校验 code 未过期 → 取出关联的 master id →
   写入 `identity_mapping (channel_ref, sender) → master_id` → code 失效。

能拿到 code 即证明此人同时控制 master 账号与从渠道账号。

- `/unbind`：在从渠道输入，删除当前 `(channel_ref, sender)` 的映射。
- 在 master 渠道输入 `/bind <code>` 无意义（master sender 本就是 person_id）；按错误用法处理。

## 5. 架构定位：基础能力（session 归一层）

统一渠道**不是** `Channel` 实体，也不出现在 `ChannelsConfig`。各子渠道照常
`listen` 收、`send` 回；"统一"只发生在 **session/history 层**。
agent 回复天然使用当前 `msg.reply_target` 回到来源子渠道，无需 send 路由。

## 6. 核心机制：入口处 key 归一

```
消息 ──▶ conversation_history_key(msg)         // 纯函数，保持不变，得到 base_key
        ──▶ resolve_session_key(msg, resolver):
              ├ 群聊 (is_group_reply_target)          → base_key
              ├ resolver 为 None（未配 master）        → base_key
              ├ master 渠道消息:
              │     sender ∈ unified_member            → "unified_<sender>"
              │     否则                                → base_key
              ├ 从渠道消息:
              │     identity_mapping 命中 master_id
              │       且 master_id ∈ unified_member    → "unified_<master_id>"
              │     否则（未绑定/陌生人）               → base_key
        ──▶ SqliteSessionBackend.load/append(key)      // 后端完全不动
```

要点：

- **不改 `conversation_history_key` 签名**（被 6+ 处调用、有大量测试）。新增包装
  `resolve_session_key(msg, resolver) -> String`：先调纯函数拿 `base_key`，再归一。
- 仅**真正读写历史**的核心路径（如 `mod.rs:3667` 一带）切换到 `resolve_session_key`；
  debounce key（`mod.rs:5188`）、运行时命令路由（`mod.rs:2167`）保持 `base_key`。
- 统一 key 形如 `unified_<master_id>`，再过 `sanitize_session_key`，命名空间隔离。

### 私聊判断

复用 `is_group_reply_target(reply_target)`（`mod.rs:2411`，当前判定
`@g.us` / `group:` 前缀，另有 `wecom_ws` 的 `group--`）。
**实现期需补充**：核对 dawnim / lark / qq / wechat 的群聊 `reply_target` 形态并扩展该判断，
避免群聊误并入私聊统一会话。

## 7. 新增模块：`zeroclaw-infra::identity_store`

独立 SQLite `{workspace}/sessions/identity.db`，与 session 后端解耦。

```sql
-- 从渠道映射：(子渠道 ChannelRef, 渠道内 sender) → master_id，由 /bind 填充
CREATE TABLE IF NOT EXISTS identity_mapping (
    channel_ref TEXT NOT NULL,   -- "<type>.<alias>"
    sender      TEXT NOT NULL,
    master_id   TEXT NOT NULL,
    PRIMARY KEY (channel_ref, sender)
);
CREATE INDEX IF NOT EXISTS idx_identity_master ON identity_mapping(master_id);

-- 启用合并的 master id 白名单，superusers 首次初始化写入
CREATE TABLE IF NOT EXISTS unified_member (
    master_id TEXT PRIMARY KEY
);
```

`/bind` code 不入库，存内存 `HashMap<code, (master_id, expires_at)>`。

### 接口

```rust
pub trait IdentityResolver: Send + Sync {
    /// master 渠道消息：sender ∈ 白名单 → Some(sender)。
    /// 从渠道消息：identity_mapping 命中且 master_id ∈ 白名单 → Some(master_id)。
    fn resolve(&self, channel_ref: &str, sender: &str, is_master: bool) -> Option<String>;
}
```

`SqliteIdentityStore` 实现 `IdentityResolver`，并提供 `bind` / `unbind` /
`issue_code` / `redeem_code` / `seed_superusers` / `list` 等方法。

## 8. 接线

- `ChannelRuntimeContext` 增加 `identity_resolver: Option<Arc<dyn IdentityResolver>>`
  与 `master_channel: Option<String>`（仿现有 `session_store`）。
- 启动构建：若配置了 `master_channel`，构建 `SqliteIdentityStore`、`seed_superusers`、注入。
- 命令解析新增 `/bind`、`/unbind`（仿 `/new`、`/stop`）。

## 9. 数据流（示例）

master = `dawnim.work`，superuser = `u_alice`。

1. alice 在 dawnim 私聊：`channel_ref=dawnim.work, sender=u_alice, is_master=true`。
   `u_alice ∈ unified_member` → `session_key=unified_u_alice`。
2. alice 在 lark 首次输入 `/bind`（在 dawnim 侧先 `/bind` 拿到 code，再在 lark `/bind <code>`）
   → 写 `(lark.work, ou_aaa) → u_alice`。
3. 此后 alice 在 lark 私聊：查映射 → `u_alice` → `session_key=unified_u_alice`，
   读到 dawnim 的历史，上下文连贯。回复经 `msg.reply_target` 回到 lark。

## 10. 边界与错误处理

- resolver 异常 / `identity.db` 不可用 → 回退 `base_key`，**绝不阻断消息**。
- 未配 `master_channel` → resolver 为 `None`，与现状一致。
- 群聊、未绑定 sender、白名单外 id → `base_key`。
- `/bind` code 过期或无效 → 提示重新获取，不写映射。
- 合并会话的 `session_metadata.sender_id` 写 `master_id`，`channel_id` 标为 `unified`。

## 11. 兼容性

- 未配 `master_channel` 时零影响。
- `conversation_history_key` 纯函数及其全部测试不变。
- `SqliteSessionBackend` / `SessionBackend` trait 不变。

## 12. 测试策略

- `identity_store` 单测：`bind`/`unbind`/`resolve` 往返；白名单校验；
  `issue_code`/`redeem_code`（含过期、一次性失效）；`seed_superusers` 幂等。
- `resolve_session_key` 单测：master 命中、从渠道命中、未绑定回退、群聊回退、resolver 为 None。
- 命令单测：`/bind`（master 侧发码、从渠道侧消费）、`/unbind`、错误用法。
- 集成测试：dawnim(master) + lark 绑定后同一 `session_key`、历史互通；
  未绑定 lark 用户隔离；群聊不归一。

## 13. 开放 / 可调项

- master 配置：单值 `master_channel`（主推）vs per-channel `master` flag。
- 来源标注：默认不标注；如需 agent 区分来源，后续可在 content 注入 `[来自 lark]` 或扩展存储。
- `/bind` code 长度、TTL 具体取值。
- `superusers` 放 `[channels]`（当前）vs 放 master 渠道配置块。
