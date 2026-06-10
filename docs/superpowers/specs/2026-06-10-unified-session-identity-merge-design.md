# 统一会话：跨端身份合并设计

- 日期：2026-06-10
- 状态：待评审
- 关联分支：0.8.0

## 1. 背景

ZeroClaw 当前的会话历史按 `session_key` 隔离。`session_key` 由
`conversation_history_key(msg)`（`crates/zeroclaw-channels/src/orchestrator/mod.rs:472`）
这个纯函数计算，构成大致为：

```
{channel}.{alias}_{reply_target}_{sender}          // 普通
{channel}.{alias}_{reply_target}_{thread_ts}_{sender}  // 线程/论坛
```

这个 key 同时是三处的标识：

1. 内存中 conversation history cache 的 HashMap key；
2. SQLite session 后端（`{workspace}/sessions/sessions.db`）`sessions` 表与
   `session_metadata` 表的 `session_key` **列**（不是表名）；
3. memory backend 的 `session_id` 过滤字段。

因为 key 以 `channel` 开头，不同渠道天然算出不同 key——这是 by-design 的渠道隔离。

## 2. 目标

同一个真人在多个**已配置子渠道**（dawnim / lark / wecom / qq / wechat 等）的
**1:1 私聊**会话历史，合并到一个统一会话，使 agent 跨端看到连贯上下文。

## 3. 非目标（明确排除）

- **不**做群聊合并：仅 1:1 私聊。群聊是多人共享空间，跨端"同一个人"语义复杂且易串上下文。
- **不**迁移存量历史：仅对启用后的新消息生效；旧的分散会话保持原样。
- **不**合并长期 memory：本期只合并会话历史（session 层）。memory 层归并是后续可选项。
- **不**做自动身份识别：跨端身份靠显式映射（数据库表），不依赖手机号/邮箱/unionid 等自动对齐。
- **不**把统一渠道做成独立 Channel：见 §4。

## 4. 架构定位：基础能力（session 归一层）

统一渠道**不是**一个实现 `Channel` trait 的实体，也不出现在 `ChannelsConfig` 中。
各子渠道照常 `listen` 收、`send` 回；"统一"只发生在 **session/history 层**——
计算 `session_key` 时把同一个人的消息归到同一个 key。

由此：

- 子渠道收发逻辑无感、零改动；
- agent 回复天然使用当前 `msg.reply_target`，回到来源子渠道，无需 send 路由；
- 改动面集中、风险低。

## 5. 核心机制：入口处 key 归一

```
消息 ──▶ conversation_history_key(msg)         // 纯函数，保持不变，得到 base_key
        ──▶ resolve_session_key(msg, resolver):
              ├ 群聊 (is_group_reply_target)        → base_key（不归一）
              ├ resolver 为 None / 未启用            → base_key
              ├ 查映射 (channel_ref, sender) → person?
              │     命中且 channel_ref 已登记为成员  → "unified_<person_id>"（经 sanitize）
              │     未命中（陌生人）                 → base_key
        ──▶ SqliteSessionBackend.load/append(key)   // 后端完全不动
```

要点：

- **不改 `conversation_history_key` 的签名**。它被 6+ 处调用且有大量测试。新增一个包装
  `resolve_session_key(msg, resolver) -> String`，先调纯函数拿 `base_key`，再经 resolver 归一。
- 只有**真正读写历史**的核心路径（如 `mod.rs:3667` 一带）切换到 `resolve_session_key`；
  debounce key（`mod.rs:5188`）、运行时命令路由（`mod.rs:2167`）保持 `base_key` 即可，
  它们不涉及历史合并。
- 新历史天然落到统一 key，无需迁移、无改表、无并发改写风险。

### 私聊判断

复用现有 `is_group_reply_target(reply_target)`（`mod.rs:2411`），当前判定为
`reply_target.contains("@g.us") || reply_target.starts_with("group:")`，另有
`wecom_ws` 的 `group--` 前缀单独处理。

**实现期需补充**：dawnim / lark / qq / wechat 的群标识可能不在上述集合内。实现时需核对
各目标渠道的群聊 `reply_target` 形态并扩展该判断，避免把群聊误并入私聊统一会话。

## 6. 新增模块：`zeroclaw-infra::identity_store`

独立 SQLite 存储 `{workspace}/sessions/identity.db`，与 session 后端解耦
（职责分离：身份映射不属于 `SessionBackend`）。

### 数据模型

```sql
-- 谁是谁：(子渠道 ChannelRef, 渠道内 sender) → person
CREATE TABLE IF NOT EXISTS identity_mapping (
    channel_ref TEXT NOT NULL,   -- "<type>.<alias>"，如 "lark.work"
    sender      TEXT NOT NULL,   -- 渠道原生 sender id
    person_id   TEXT NOT NULL,
    PRIMARY KEY (channel_ref, sender)
);
CREATE INDEX IF NOT EXISTS idx_identity_person ON identity_mapping(person_id);

-- 声明哪些子渠道参与某 person 的合并（控制范围 + 显式启用）
CREATE TABLE IF NOT EXISTS unified_member (
    person_id   TEXT NOT NULL,
    channel_ref TEXT NOT NULL,
    PRIMARY KEY (person_id, channel_ref)
);
```

`unified_member` 让"哪些渠道纳入合并"可显式控制：仅当命中映射且该 `channel_ref`
登记为该 person 的成员时才归一。这样能精细到"某人只合并 lark+wecom，不合并 qq"。

### 接口

```rust
pub trait IdentityResolver: Send + Sync {
    /// 命中且 channel_ref 是该 person 的登记成员 → Some(person_id)。
    fn resolve(&self, channel_ref: &str, sender: &str) -> Option<String>;
}
```

`SqliteIdentityStore` 实现 `IdentityResolver`，并提供管理方法
（`link` / `unlink` / `list` / `add_member` / `remove_member`）。

## 7. 接线

- `ChannelRuntimeContext` 增加 `identity_resolver: Option<Arc<dyn IdentityResolver>>`，
  仿现有 `session_store` 字段。
- 启动构建时，若配置开关开启则构建 `SqliteIdentityStore` 注入；否则为 `None`
  （行为与现状 100% 一致）。
- `resolve_session_key` 放在 orchestrator，供核心历史路径调用。

## 8. 配置

唯一新增配置项为总开关：

```toml
[identity]
unified_sessions = true   # 默认 false：关闭时 resolver 为 None，零行为变化
```

映射数据本身**不进 config**，存于 `identity.db`，通过 CLI 动态维护（见 §9）。

## 9. 管理 CLI

```
zeroclaw identity link   <person> <channel_ref> <sender>   # 录入映射，并登记成员
zeroclaw identity unlink <channel_ref> <sender>            # 删除一条映射
zeroclaw identity list   [--person <id>]                   # 查看
```

`link` 在写入 `identity_mapping` 的同时，向 `unified_member` 登记
`(person_id, channel_ref)`，使该渠道默认纳入合并。

> 可选后续扩展（非本期）：REST 端点 `/api/identity` 做批量导入；本期仅 CLI。

## 10. 数据流（示例）

1. 张三在 lark.work 私聊发消息：`channel_ref=lark.work, sender=ou_aaa`。
   `resolve` 命中 `person=zhangsan` → `session_key=unified_zhangsan`。历史写入此 key。
2. 张三转到 wecom.team 私聊：`channel_ref=wecom.team, sender=wx_bbb` →
   同样解析到 `unified_zhangsan` → 读到步骤 1 的历史，上下文连贯。
3. agent 回复使用步骤 2 的 `msg.reply_target` → 回到 wecom.team。

## 11. 边界与错误处理

- resolver 查询异常 / `identity.db` 不可用 → 回退 `base_key`，**绝不阻断消息**。
- 未配置或开关关闭 → resolver 为 `None`，与现状完全一致。
- 群聊、未登记 sender → `base_key`（陌生人按单渠道隔离）。
- 统一 key 经 `sanitize_session_key` 处理，与 on-disk / 列值规则一致。

## 12. 兼容性

- 默认关闭，存量部署零影响。
- `conversation_history_key` 纯函数及其全部测试不变。
- `SqliteSessionBackend` / `SessionBackend` trait 不变。

## 13. 测试策略

- `identity_store` 单测：`link`/`unlink`/`resolve` 往返、未命中、成员未登记不归一。
- `resolve_session_key` 单测：命中归一、陌生人回退、群聊回退、resolver 为 None。
- 集成测试：lark + wecom 两条私聊（同一 person）→ 同一 `session_key`、历史互通；
  不同 person → 隔离；群聊不归一。

## 14. 命名约定（可调）

- 统一 key 形如 `unified_<person_id>`（再过 `sanitize_session_key`）。
- `person_id` 由运维在 `link` 时指定（如拼音、工号），需保证全局唯一。
