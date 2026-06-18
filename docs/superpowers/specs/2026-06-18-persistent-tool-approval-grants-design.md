# 持久化按 topic 维度的工具审批授权设计

> 日期：2026-06-18
> 状态：✅ 设计已与维护方确认，待 implementation plan
> 关联：
> - 复用 [DawnIM 多话题映射](2026-06-14-dawn-im-multi-topic-design.md)（`thread_ts` → topic）
> - 复用既有身份绑定（`/bind` / `/unbind` + `IdentityResolver`）以及 `ChannelsConfig.superusers`
> - 依赖现有 `ApprovalManager`、`approval_gate.rs`、`Channel::request_approval` 契约
> - 关联 PR #50（审批卡片路由到正确 topic 线程）

## 1. 背景与问题

当前 ZeroClaw 的工具审批由 `crates/zeroclaw-runtime/src/approval/mod.rs` 的
`ApprovalManager` 承担，它只在内存中维护两份状态：

- `session_allowlist: Mutex<HashSet<String>>` —— 仅按 `tool_name` 维度，**进程内 session 级**
- `audit_log: Mutex<Vec<ApprovalLogEntry>>` —— 仅内存，不落盘；并且 `zeroclaw-log` 早已为 `record!` 事件提供 `runtime-trace.jsonl` 统一持久化，二者并存属于 **duplicate state** 风险（AGENTS.md DRY 铁律禁止）

导致两个用户痛点：

1. **重复审批**：在同一个 topic 内反复调用同一个工具，每次都弹卡；非技术用户体验差
2. **重启即丢**：daemon 重启或部署变更后所有 `Always` 选择失效，必须重新逐个审批
3. **卡片晦涩**：dawn_im 等渠道的审批卡片直接拼 `tool_name + arguments_summary`，对非技术用户难以理解

此外，运维侧缺乏对"已审批清单"的可观察性：管理员看不到哪些 (user, tool, topic) 已被授权，
也无法显式撤销某条授权。

## 2. 设计目标

- **保留单次允许 + 新增始终允许**：审批卡片仍有「同意」（仅本次放行）按钮；额外增加「始终允许」按钮 — 只有点击后者才落 grant 长期免审
- **同 topic + 同工具 → 一次始终允许长期免审**：键为 `(channel_ref, topic, triggerer_master_id, tool_name)`
- **持久化**：审批结果落 sqlite，重启后仍有效
- **可撤销**：通过 gateway API 显式查看/删除已存授权
- **审批人解耦**：审批人始终是 `ChannelsConfig.superusers`；触发者不是 superuser 时由 broker 将卡片代发给 superuser
- **多 superuser 先回为准**：并发广播给所有 superuser，第一个非超时回复决出结果，其他卡片被通知失效
- **卡片人话化**：用轻量 LLM 把工具调用摘要为友好中文，失败回退到现有原文
- **DRY**：superuser 列表只读 `ChannelsConfig.superusers`、反向 uid 解析复用 `/bind` 表，**不在新表里复制状态**

## 3. 非目标与前提假设

非目标：

- **不**给 grant 加 TTL 自动过期：撤销走 gateway DELETE，与 audit_log 的不可篡改语义一致
- **不**对历史 audit_log 做迁移：保留旧表（如果存在），新设计从 0 开始累积
- **不**支持跨 daemon 实例共享 grant（多进程协调不在本次范围）
- **不**改 `Tool trait` 与 `Channel trait` 的方法签名；只新增 channel 端的可选 `cancel_approval(approval_id, reason)` 默认空实现
- **不**做 channel-side 卡片"已由 XX 处理"更新的强约束 — best-effort，channel 不支持就不更新

前提假设：

- 部署设置了 `master_channel` 且 `superusers` 非空 — 否则非 superuser 触发的代审一律拒绝（详见 §9）
- `IdentityResolver` 的底层 sqlite 表能在不复制数据的前提下支持反向查询 `(master_id, target_channel_ref) → channel_uid`
- LLM 摘要 provider 失败/超时是常态，必须有 fallback
- **`ChannelOrigin.topic` 已是 effective_topic**（orchestrator `:5002` 现已传 `effective_topic`，涵盖 `msg.thread_ts` 与 `/topic` 绑定回退；与 `resolve_session_key` 内部一致）— broker 直接读用即可，**不再自行计算 topic**
- **`ChannelOrigin` 需新增 `triggerer_master_id: Option<String>` 字段**：orchestrator 在拼 `channel_origin` 时把 `resolve_session_key` 内部已调的 `identity.resolver.resolve(...)` 结果一并提到外层变量，赋给该字段；broker 直接读用，**不再自行解析 master_id**（DRY）

## 4. 用户场景

| 场景 | 设置 | 行为 |
|------|------|------|
| **S1：superuser 单聊触发** | superuser 在自己与 ZeroClaw 的 dawn_im 单聊中调用 shell | 在该单聊弹卡，superuser 自审；选 `Always` 后落 grant；同 (channel, topic, user, tool) 后续免审 |
| **S2：superuser 群聊触发** | superuser 在 dawn_im 群里某 topic 中调用 shell | 在群内该 topic 弹卡，superuser 自审；落 grant 后该 topic 内同工具免审；其它 topic 仍要审 |
| **S3：非 superuser 群聊触发** | 普通用户 u_alice 在群聊某 topic 中触发工具 | broker 反向解析所有 superuser 在群所在 channel 的 uid（查不到回退 master_channel）；同时私聊广播；第一个回复获胜；卡片含「u_alice 在 #db_lock 想执行 …」 |
| **S4：重启后再次触发** | 进程重启 | `SqliteGrantStore` 命中重启前写下的 grant，无需再审，仍写 audit log |
| **S5：运维显式撤销** | 管理员发现 (u_alice, shell) 不该常驻 | `DELETE /api/approvals/grants/{id}`；下次 u_alice 在同 topic 调 shell 会再次弹卡 |
| **S6：LLM 摘要 provider 宕** | 卡片人话化失败 | 卡片回退到 `tool_name + arguments_summary` 原文；审批继续 |

## 5. 关键设计决策

| # | 维度 | 决定 |
|---|---|---|
| 0 | 按钮语义 | 卡片三按钮：**同意（单次，不写 grant，保留现状）** / **始终允许（新增，写 grant）** / **拒绝（保留现状）** |
| 1 | 持久化键 | `channel_ref + topic + triggerer_master_id + tool_name`（仅「始终允许」写入此键） |
| 2 | 审批人来源 | 复用 `ChannelsConfig.superusers`（master uid list），其字段注释从 `/bind` 专用扩展为「全局审批人 + `/bind` 白名单」 |
| 3 | 触发者识别 | 用现有 `IdentityResolver` 正向解析为 master_id |
| 4 | 送达路由 | 优先在触发所在 channel 私聊 superuser；查不到该 channel 的 uid 就回退 master_channel |
| 5 | 多 superuser | 并发发卡，**先回为准**；其它候选 best-effort 取消（更新卡为「已由 XX 处理」） |
| 6 | 卡片内容 | 轻量 LLM 摘要（可配置 provider），失败回退到现有 `tool_name + arguments_summary`；硬超时 **10 秒** |
| 7 | 卡片身份 | 含「{triggerer_display_name} 在 [{channel_ref} / #{topic}] 想执行：{humanized}」 |
| 8 | 存储后端 | `zeroclaw-memory::SqliteMemory`，新增表 `approval_grants` |
| 9 | Gateway API | 新增 `GET /api/approvals/grants` + `DELETE /api/approvals/grants/{id}` |
| 10 | 超时 | 沿用现有 `approval_timeout_secs`，超时默认拒绝 |
| 11 | DRY | superuser 唯一源仍是 `ChannelsConfig.superusers`；反向 uid 解析查 `/bind` 表；删除旧 `session_allowlist: Mutex<HashSet<String>>` 字段，不双写 |
| 12 | audit 持久化 | **复用 zeroclaw-log**：审批事件通过 `record!(...)` 落到 `<workspace>/state/runtime-trace.jsonl`，附 `EventCategory::Tool` + `Action::Approve` / `Action::Reject`；**删除内存 `audit_log: Mutex<Vec<ApprovalLogEntry>>` 字段**；不引入独立 `audit.jsonl`（避免 duplicate state） |

## 6. 架构总览

```
┌──────────────────────────────────────────────────────────────────┐
│ runtime / agent / turn / approval_gate.rs                        │
│   1) ApprovalManager.requirement(tool)                           │
│   2) 若 Approved（auto_approve 命中或 grant 命中）→ Proceed       │
│   3) 否则 → ApprovalBroker.request_decision(ctx)                 │
└──────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────┐
│ runtime / approval / broker.rs   (新增)                          │
│   1. classify_role(triggerer_master_id)                          │
│        Self    → 触发者本人是 superuser → 自我审批                 │
│        Proxy   → 非 superuser → 代审                              │
│   2. resolve_superuser_targets(channel_ref)                      │
│        - 调用 IdentityResolver.reverse_lookup(master_id, channel)│
│        - 查不到回退 master_channel                                │
│   3. humanize_card(tool, summarized_args, triggerer_name)        │
│        - 调用 SummaryProvider，超时/失败 fallback                  │
│   4. fan_out_approval()                                          │
│        - JoinSet 并发对每个 target 调 Channel::request_approval  │
│        - 第一个 Ok(Some(_)) 即 winner；其它 cancel_approval       │
│   5. record_decision_and_persist_grant()                         │
│        - audit_log 写入                                          │
│        - decision == Always → ApprovalGrantStore.put             │
└──────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────┐
│ runtime / approval / grant_store.rs   (新增)                     │
│   trait ApprovalGrantStore                                       │
│      get(channel,topic,user,tool) -> Option<ApprovalGrant>       │
│      put(grant)                                                  │
│      list(filter) -> Vec<ApprovalGrant>                          │
│      delete(grant_id) -> bool                                    │
│                                                                  │
│   SqliteGrantStore: 包一层 SqliteMemory("approval_grants")       │
│      + 内存 LRU(1024) 缓存（同时缓存 hit 与 miss）                 │
└──────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌──────────────────────────────────────────────────────────────────┐
│ gateway / api.rs   (扩展)                                        │
│   GET    /api/approvals/grants  ?channel=&topic=&user=&tool=    │
│   DELETE /api/approvals/grants/{id}                              │
│   依赖注入: Arc<dyn ApprovalGrantStore>                          │
└──────────────────────────────────────────────────────────────────┘
```

**关键边界承诺**：

- `Tool trait` 不动
- `Channel trait` 不动；新增的 `cancel_approval` 是带默认空实现的可选方法
- `ApprovalManager.session_allowlist` 字段**删除**，由 `Arc<dyn ApprovalGrantStore>` 取代；不双写

## 7. 数据模型

### 7.1 `ApprovalGrant`（in-process 结构）

位于 `crates/zeroclaw-runtime/src/approval/grant_store.rs`：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalGrant {
    pub id: String,                  // ULID（lexicographic 排序友好）
    pub channel_ref: String,         // "<type>.<alias>", e.g. "dawnim.work"
    pub topic: Option<String>,       // None = 无 topic
    pub user_master_id: String,      // 触发者 master_id
    pub tool_name: String,
    pub granted_at: i64,             // UTC unix seconds
    pub granted_by_master_id: String,// 实际点 Always 的 superuser
    pub granted_via_channel: String, // superuser 点 Always 时所在 channel_ref
}
```

字段为什么这样选：

- `id`：gateway DELETE 用；ULID 时间有序，列表查询天然按时间倒序
- `topic: Option<String>`：与 `ChannelOrigin.topic` 语义一致；`None` 不映射到空串
- `user_master_id`：所有 channel 解析到同一身份，避免"换 channel 又要再审"
- `granted_by` / `granted_via`：审计 + gateway list 展示

### 7.2 SQLite 表

复用 `zeroclaw-memory::SqliteMemory`，新命名 `approval_grants`：

```sql
CREATE TABLE IF NOT EXISTS approval_grants (
    id                     TEXT PRIMARY KEY,
    channel_ref            TEXT NOT NULL,
    topic                  TEXT,                    -- NULL = 无 topic
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
    ON approval_grants (user_master_id);
```

实现注意：

- `INSERT ... ON CONFLICT(channel_ref,topic,user_master_id,tool_name) DO UPDATE SET granted_at=excluded.granted_at, granted_by_master_id=excluded.granted_by_master_id, granted_via_channel=excluded.granted_via_channel` — 二次 Always 刷新审计字段而不重复行
- `topic IS NULL` 与 `topic = ?` 必须在 store 实现层正确分流

### 7.3 缓存策略

`SqliteGrantStore` 内部：

- `Mutex<LruCache<(String, Option<String>, String, String), Option<ApprovalGrant>>>`
- 容量 1024（足以覆盖典型部署）
- **同时缓存命中与未命中**：避免高频未授权工具反复打 DB
- 写路径（`put` / `delete`）穿透 sqlite 后主动 invalidate 对应 key
- gateway DELETE 使用同一 `Arc<dyn ApprovalGrantStore>` 实例，缓存一致

### 7.4 Gateway API 响应格式

```jsonc
// GET /api/approvals/grants?channel=&topic=&user=&tool=
[
  {
    "id": "01JE...",
    "channel_ref": "dawnim.work",
    "topic": "db_lock",
    "user_master_id": "u_alice",
    "tool_name": "shell",
    "granted_at": 1750252800,
    "granted_by_master_id": "u_admin",
    "granted_via_channel": "dawnim.work"
  }
]

// DELETE /api/approvals/grants/{id}
// 200 OK { "deleted": true }
// 404 Not Found { "deleted": false }
```

- 过滤参数全部可选，空过滤等于全部
- 列表按 `granted_at DESC` 返回

## 8. 数据流

### 8.1 完整流程

```
tool_call → approval_gate
   ├─ ApprovalManager.approval_requirement(tool)
   │     ├─ auto_approve / Full → Proceed(approved=true)
   │     ├─ ReadOnly → NotRequired
   │     ├─ always_ask 命中 → 强制走 Broker（跳过 grant 查询）
   │     └─ 其他 → 走 Broker
   ▼
ApprovalBroker.request_decision(ctx)
   ├─ GrantStore.get(channel, topic, user_master_id, tool)
   │     └─ 命中 → 写 audit_log(reason="cached_grant") → Proceed
   ├─ 未命中 →
   │     ├─ is_superuser(triggerer)?
   │     │     ├─ 是 → self-approval：在触发所在 channel 弹卡（沿用 PR #50 行为）
   │     │     └─ 否 → proxy-approval：
   │     │             1. resolve_superuser_targets(channel_ref)
   │     │             2. humanize_card(...)   (≤10s, fallback)
   │     │             3. fan_out 并发 request_approval
   │     │             4. first non-timeout reply wins; others cancel
   │     │             5. 全部失败 / 超时 → Decision::No (audit reason)
   │     ▼
   │   decision == Always?
   │     ├─ 是 → GrantStore.put(...)
   │     └─ 否 → 不写 grant
   ▼
audit_log.append(decision, reason)
return Approve / Deny / Replace
```

### 8.2 三种典型场景细化

**场景 A — superuser 单聊触发**：
- self-approval；卡片文案「你即将执行 …」（不展开"XXX 用户"前缀）
- decision 用触发者本人即 superuser 自己；grant 写入 `granted_by_master_id = user_master_id`

**场景 B — superuser 群聊触发**：
- self-approval；卡片落在群内当前 topic（PR #50 已实现该行为）
- 卡片文案「{display_name} 在 #{topic} 想执行 …」（与场景 C 一致）

**场景 C — 非 superuser 触发**：
- proxy-approval；对每个 superuser 通过 `IdentityResolver.reverse_lookup(master_id, channel_ref)`
  查找该 channel 上的 uid；查不到的 superuser 改投到 master_channel
- 卡片按钮三选一：「同意（单次）/ 始终允许 / 拒绝」（即 Approve / AlwaysApprove / Deny）
  - **「同意」语义保持不变**：仅本次放行，**不写 grant**，下次同键调用仍要再审
  - **「始终允许」是新增按钮**：本次放行 + 写 grant，键 `(channel,topic,user,tool)` 下次免审；这是持久化授权的唯一入口
  - **「拒绝」语义保持不变**：本次拒绝，不写 grant
  - 现状：`crates/zeroclaw-channels/src/dawn_im/approval.rs` 的 `build_approval_card` 只渲染「同意/拒绝」两个按钮，未暴露 `AlwaysApprove`；如果不加「始终允许」按钮，grant 永远写不进库，持久化免审失效
  - 本次范围：扩展该卡片**新增**第三按钮（value=`always`，置于「同意」与「拒绝」之间），channel inbound handler 解析 `always` 映射到 `ChannelApprovalResponse::AlwaysApprove`；原有 `approve` / `deny` 按钮和解析路径**保持不变**
  - lark 现状已具备三按钮（`approve` / `deny` / `always`）+ `build_resolved_approval_card` patch 能力，本次不需改 lark；其它新增 `request_approval` 的 channel 自行参考 lark
- 第一个 `Ok(Some(_))` 即 winner；其它仍 in-flight 的目标调用 `Channel::cancel_approval(approval_id, "已由 {decider} 处理")`
- channel 端 cancel 默认空实现；dawn_im 等支持卡片更新的实现真正更新卡片文案

### 8.3 反向 uid 解析

`IdentityResolver` 现有正向方法 `(channel_ref, sender) → master_id`，数据由 `/bind` 写入同一 sqlite。

**做法**：在现有 identity store 上新增 `reverse_lookup(master_id, channel_ref) -> Option<String>`，
读同一张 binding 表（按需加反向 index）。**不复制到新表**，遵守 DRY。

如果某 superuser 在触发 channel 上从未 `/bind`：

- target channel = `config.channels.master_channel`
- target uid = `master_id` 本身（master_channel 上 uid 即 master_id）

### 8.4 audit 写入策略（无条件，全路径，单一接收端 = zeroclaw-log）

**所有审批结果通过 `record!` 统一落 `runtime-trace.jsonl`，不漏一条；不双写到内存或独立文件**：

| 决定 / 路径 | `Action` | `outcome` | `reason` 字段值 | 是否写 grant |
|---|---|---|---|---|
| 单次同意（用户点「同意」） | `Action::Approve` | `Success` | `"interactive_approve"` | 否 |
| 始终允许（用户点「始终允许」） | `Action::Approve` | `Success` | `"interactive_always"` | **是** |
| 拒绝（用户点「拒绝」） | `Action::Reject` | `Failure` | `"interactive_deny"` | 否 |
| 替换（操作员改写参数） | `Action::Reject` | `Failure` | `"interactive_replace"` | 否 |
| grant 命中免审 | `Action::Approve` | `Success` | `"cached_grant"` + `grant_id` | 否 |
| 所有 superuser 超时 | `Action::Reject` | `Failure` | `"all_superusers_timeout"` | 否 |
| 所有目标 channel 失败 | `Action::Reject` | `Failure` | `"all_channels_failed"` | 否 |
| `superusers` 为空 | `Action::Reject` | `Failure` | `"no_superuser_configured"` | 否 |
| `master_channel` 缺失 | `Action::Reject` | `Failure` | `"no_master_channel"` | 否 |
| `auto_approve` 直放 | `Action::Approve` | `Success` | `"policy_auto_approve"` | 否 |
| `Full` autonomy 直放 | `Action::Approve` | `Success` | `"policy_autonomy_full"` | 否 |

每条事件**统一携带**以下结构化字段（便于 LogFilter 过滤）：

```json
{
  "category": "tool",
  "action": "approve|reject",
  "outcome": "success|failure",
  "reason": "...",
  "tool": "<tool_name>",
  "channel": "<deciding_channel_ref>",
  "channel_ref": "<triggering_channel_ref>",
  "topic": "<topic_or_null>",
  "user_master_id": "<triggerer>",
  "granted_by_master_id": "<superuser_if_proxy_path>",
  "grant_id": "<ulid_if_cached_grant_hit>",
  "arguments_summary": "<已脱敏的概要文本>"
}
```

实现位置：

- 所有审批决定都通过新的 `ApprovalManager.record_decision(tool, args, decision, channel, reason, extras)` 调用；该函数内部唯一动作就是 `record!(INFO/WARN, Event::new(...).with_category(EventCategory::Tool), ...)`
- **不再持有** `Mutex<Vec<ApprovalLogEntry>>` 内存字段；`audit_log()` 访问器一并删除（生产代码 0 调用，仅测试用，测试改写为捕获 log）
- broker 与 approval_gate 任何一条 return 之前都必经 `record_decision`；用代码 review 兜底（PR 模板自检项）

EventCategory 复用现有 `Tool` 变体（审批属于 tool 执行前置环节），**不动 `zeroclaw-log` 的 `EventCategory` 枚举**（避免触动 Beta crate）。

### 8.5 audit 查询入口

- 命令行 / 调试：`tail <workspace>/state/runtime-trace.jsonl | jq 'select(.action=="approve" or .action=="reject")'`
- 编程接口：`zeroclaw_log::reader::load_page(LogFilter { action: Some("approve" | "reject"), ... })`
- Gateway：本次**不新增** `/api/approvals/audit` 端点；如需要可借助现有 `/api/logs` 加 action 过滤（如果未实现，列入 §11 非本次范围）

### 8.6 安全 / 隐私（与 4.x 节呼应）

- `arguments_summary` 字段送入 jsonl **必须先经 `summarize_args` 红线脱敏**；jsonl 是持久化文件，泄露面比内存大
- 不写 `raw_arguments` 到 jsonl
- 不写 LLM 摘要后的卡片文本到 jsonl（避免与原 arguments_summary 重复）

## 9. 错误处理与边界条件

### 9.1 失败矩阵

| 失败/边界 | 处理 | 依据 |
|---|---|---|
| `superusers` 为空 | broker 拒绝；audit reason=`no_superuser_configured` | 安全默认，不静默放行 |
| `master_channel` 未配置 | 同上拒绝；启动时 `tracing::warn` 一次 | superuser 概念依赖 master_channel |
| 触发者 master_id 解析失败 | 视为非 superuser 走代审 | 不能因解析失败就当 superuser |
| 反向解析在所有 channel 失败 | 回退 master_channel uid（master_id 本身） | 最坏情况一定能送到 master_channel |
| 目标 channel `request_approval` 返回 `Err` | 该 target 视作超时剔除；全部失败 → Deny，reason=`all_channels_failed` | 不让一个 channel 拖垮全流程 |
| channel 返回 `Ok(None)`（不支持 approval） | 同上剔除 | 沿用现有契约 |
| 全部 superuser 超时未回复 | Deny，reason=`all_superusers_timeout`；卡片显示「审批超时，已自动拒绝」 | 与现有 `approval_timeout_secs` 一致 |
| LLM provider 失败 / 不可用 | fallback 到 `tool_name + arguments_summary`；`tracing::debug` | 可读性次要、可用性优先 |
| LLM 摘要超时（>10s） | 同上 fallback | 不阻塞 tool 执行 |
| `GrantStore.get` 失败 | 视为未命中走审批；`tracing::error` | 保守：宁可多审也不放行 |
| `GrantStore.put` 失败（Always 但落盘失败） | 当次仍按 Approve 放行；下次仍会再问；`tracing::error` | 尊重用户当下意图；持久化失败为次要降级 |
| gateway DELETE 不存在 id | 404 `{"deleted": false}` | REST 惯例 |
| gateway DELETE 与运行中审批并发 | 不影响 in-flight；下次同键调用才重新审 | 简化语义，不引入分布式协调 |

### 9.2 配置变更的语义

| 变更 | 对已存 grant 的影响 |
|---|---|
| 从 `superusers` 移除某人 | 不主动清空 TA 之前签发的 grant；显式撤销走 gateway DELETE |
| tool 加入 `auto_approve` | grant 失效（policy 在 grant 之前命中放行），记录留着不删 |
| tool 加入 `always_ask` | **优先级最高**，跳过 GrantStore 查询，必弹卡（与现有 `always_ask` 语义一致） |
| 修改 `master_channel` | 旧 grant 的 `user_master_id` 仍指原 master_channel 上的 uid；运维应先 gateway 清空 |

### 9.3 安全与隐私

- **卡片渲染**：先 `summarize_args(args)` 应用 `looks_like_secret_key` 红线 → 把脱敏后的字符串送给 LLM；**不要把 raw_arguments 直接发给 LLM**
- **LLM provider 选择**：默认走 agent 自身 provider；可在新增配置段 `[approval] summary_provider = "..."` 单独配
- **Gateway API 鉴权**：复用 gateway 现有鉴权中间件；新 endpoint 与现有 admin 级别一致，不引入新权限模型
- **Grant 表数据敏感性**：表内只存 channel_ref / topic / uid / tool_name；**不存任何参数值**
- **不持久化 display_name**：display_name 易变，运行时按需向 channel 反查

### 9.4 并发与一致性

- grant 写入 `INSERT ... ON CONFLICT DO UPDATE`，天然幂等
- 同 (channel,topic,user,tool) 同时存在两个 in-flight 审批：grant 命中检查在 fan_out 之前，理论上不会同时发起两个 fan_out；如真同时（极小窗口）— 两个都等到回复后写 grant 时后写者覆盖前者（语义可接受）

## 10. 测试策略

### 10.1 单元测试

**`runtime/approval/grant_store.rs`**

- `sqlite_store_round_trip`：put → get 命中、字段全等
- `sqlite_store_unique_constraint`：同键二次 put 是 UPSERT，`granted_at` 刷新，行数仍 1
- `sqlite_store_topic_null_lookup`：`topic = None` 的 put / get / delete 用 `IS NULL` 比对，不与 `topic = Some("")` 串味
- `sqlite_store_list_filter`：按 channel / topic / user / tool 任意组合过滤；空过滤等于全表
- `sqlite_store_delete_by_id`：删除存在返回 true，不存在返回 false
- `sqlite_store_cache_hit_miss`：mock sqlite 调用次数；连续 get 同 key 不再打 DB；put / delete 后 invalidate
- `sqlite_store_io_error_returns_none`：底层报错时 get 返回 `Ok(None)`，put 返回 `Err`

**`runtime/approval/broker.rs`**

- `triggerer_is_superuser_uses_self_flow`：recipient = 触发者 reply_target
- `triggerer_not_superuser_fans_out_to_all_superusers`：N 个 superuser 收 N 张卡
- `first_reply_wins_others_cancelled`：fake channel 用 oneshot 控制回复顺序；winner 决定；其余被 cancel
- `all_targets_timeout_denies`：reason=`all_superusers_timeout`
- `all_target_channels_failed_denies`：reason=`all_channels_failed`
- `no_superusers_configured_denies`：reason=`no_superuser_configured`
- `reverse_lookup_falls_back_to_master_channel`：触发 channel 上无 binding → 用 master_channel uid
- `cached_grant_hit_still_emits_record`：grant 命中也通过 `record!` 发事件，action=`approve`、reason=`cached_grant`，附 grant_id
- `broker_emits_record_per_decision_invariant`：参数化测试覆盖所有出口（interactive_approve / interactive_always / interactive_deny / interactive_replace / cached_grant / all_superusers_timeout / all_channels_failed / no_superuser_configured / no_master_channel），通过 `try_install_capture_subscriber` + `LogCaptureLayer` 断言每条出口 captured events 恰好 +1 且 (action, reason) 对应 §8.4 表
- `policy_auto_approve_emits_record`：tool ∈ auto_approve 直放也调用 record_decision，事件 action=`approve`、reason=`policy_auto_approve`
- `policy_full_autonomy_emits_record`：Full autonomy 直放也调用 record_decision，事件 action=`approve`、reason=`policy_autonomy_full`
- `record_carries_attribution_fields`：断言事件含 tool / channel / channel_ref / topic / user_master_id 字段；`granted_by_master_id` 仅在 proxy 路径出现；`grant_id` 仅在 cached_grant 出现
- `record_arguments_summary_is_redacted`：raw_arguments 含 api_key → 落 jsonl 的 `arguments_summary` 不含值

**测试基础设施**：所有发事件的测试持有 `zeroclaw_log::__private_test_writer_lock()` + `__private_test_hook_lock()`，避免并行测试串味；用 `try_install_capture_subscriber` 捕获事件供断言。
- `always_response_writes_grant`：decision = Always → put 被调用一次，字段全等
- `yes_response_does_not_write_grant`：decision = Yes → put 未被调用
- `always_ask_skips_grant_lookup`：tool ∈ always_ask → 跳过 get，必弹卡
- `humanize_failure_falls_back_to_plain`：mock SummaryProvider 报错 → 卡片文本 = 原文，主流程不中断
- `humanize_10s_timeout_falls_back`：SummaryProvider 延迟 11s → 在 10s 处放弃，走 fallback

**`runtime/approval/humanize.rs`**

- `humanize_strips_secret_keys_before_llm`：raw_arguments 含 `api_key` → 送给 LLM 的 prompt 不含值，含 `[redacted]`
- `humanize_truncates_long_payload`：参数超阈值时被截断后再送 LLM
- `humanize_uses_cache`：相同 (tool, arguments_summary) 连续两次只调用 provider 一次

**`runtime/approval/mod.rs`（ApprovalManager 调整）**

- `session_allowlist_removed_in_favor_of_grant_store`：编译期不再有 `session_allowlist` 字段
- `audit_log_field_removed`：编译期不再有 `audit_log: Mutex<Vec<ApprovalLogEntry>>` 字段，也不再有 `audit_log()` 访问器
- 现有 `always_response_adds_to_session_allowlist` / `non_interactive_session_allowlist_still_works` 改写成 grant 维度
- 现有 `audit_log_records_decisions` / `audit_log_contains_timestamp_and_channel` 改写为「`record!` 捕获到对应事件」
- `src/approval/mod.rs` 与 `crates/zeroclaw-runtime/src/agent/safety_net.rs` 的 `audit_log()` 测试同步重写（4 处调用点）

### 10.2 组件 / 集成测试

**`tests/component/approval_grant_persistence.rs`（新增）**

- `grant_survives_process_restart`：起 in-memory sqlite → 写 grant → drop 实例 → 重开同库 → get 命中
- `grant_isolated_by_topic`：同 (channel, user, tool) 不同 topic 互不命中
- `grant_isolated_by_user`：同 (channel, topic, tool) 不同 user 互不命中

**`tests/component/approval_routing.rs`（新增）**

- `superuser_direct_chat_uses_triggerer_recipient`：fake channel 验证 recipient = sender
- `proxy_approval_prefers_triggering_channel`：superuser 在触发 channel 有 binding → 收卡 channel = 触发 channel；无 binding → 收卡 channel = master_channel
- `proxy_approval_card_carries_triggerer_identity`：卡片文本包含 triggerer display name + topic

**`tests/component/security.rs`（扩展）**

- `grant_does_not_persist_secret_values`：断言 sqlite 行内无 api_key / password 子串

### 10.3 Gateway 端测试

**`crates/zeroclaw-gateway/src/api.rs` 内联 + `tests/`**

- `get_grants_empty`：空库返回 `[]`
- `get_grants_filter_by_channel_tool`：插 3 条 → query 过滤返回 2 条，按 granted_at DESC
- `delete_grant_existing`：返回 200 `{deleted: true}` + 后续 GET 不再含该 id
- `delete_grant_missing`：返回 404 `{deleted: false}`
- `delete_grant_invalidates_cache`：delete 后 broker 端立刻 get 返回 None（共享同一 store 实例）
- `gateway_endpoints_respect_existing_auth`：未授权请求被现有中间件拒绝

### 10.4 不测的部分

- 真实 LLM provider 调用 — 用 mock `SummaryProvider`，不联网
- 真实 dawn_im WebSocket — 用 fake `Channel` 实现
- 真实 sqlite 文件 I/O — 用 `:memory:` sqlite
- 跨进程并发竞争 — broker 内只保证单进程一致；多 daemon 部署不在本次范围

### 10.5 CI 守护

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets -- -D warnings`
- `dev/ci.sh dry-check` 保证未引入 duplicate state 违例（DRY 守护）

## 11. 范围与后续

### 本次范围

- 新增 `crates/zeroclaw-runtime/src/approval/grant_store.rs`（trait + Sqlite 实现 + LRU 缓存）
- 新增 `crates/zeroclaw-runtime/src/approval/broker.rs`（broker 主体）
- 新增 `crates/zeroclaw-runtime/src/approval/humanize.rs`（卡片人话化 + LLM 调用）
- 修改 `crates/zeroclaw-runtime/src/approval/mod.rs`：
  - 删除 `session_allowlist: Mutex<HashSet<String>>` 字段
  - 删除 `audit_log: Mutex<Vec<ApprovalLogEntry>>` 字段与 `audit_log()` 访问器
  - 注入 `Arc<dyn ApprovalGrantStore>` + broker 依赖
  - 改造 `record_decision` 内部实现为 `record!(...)` 调用（保留方法签名 + `reason: &str` 新增形参）
  - 暴露给 broker 的最小新接口
- 同步修改测试调用点：
  - `crates/zeroclaw-runtime/src/approval/mod.rs` 单元测试（2 处 `audit_log()`）
  - `crates/zeroclaw-runtime/src/agent/safety_net.rs` 测试（1 处 `audit_log()`）
  - `src/approval/mod.rs` 单元测试（2 处 `audit_log()`）
- 修改 `crates/zeroclaw-runtime/src/agent/turn/approval_gate.rs`：调用 broker，原 channel 路径下沉到 broker 内
- 修改 `crates/zeroclaw-api/src/channel.rs`：新增 `Channel::cancel_approval` 默认空实现方法
- 修改 `crates/zeroclaw-channels/src/dawn_im/approval.rs`：
  - `build_approval_card` 增加第三按钮「始终允许」（value=`always`）
  - inbound `WkApprovalAction` 解析 `always` → `ChannelApprovalResponse::AlwaysApprove`
  - 实现 `cancel_approval`（卡片更新 best-effort）
- **不修改** `crates/zeroclaw-channels/src/lark.rs`：现状已具备三按钮 + resolved patch，cancel_approval 实现可调用其现有 `build_resolved_approval_card` 走 patch 路径
- 修改 `crates/zeroclaw-config/src/schema.rs`：
  - 更新 `ChannelsConfig.superusers` 的字段注释（从 `/bind` 专用扩展为「全局审批人 + `/bind` 白名单」）
  - 新增 `[approval]` 配置段（含 `summary_provider`）
- 修改 `crates/zeroclaw-infra/src/`（identity store 实现位置，依据 `zeroclaw_infra::make_identity_store`）：
  新增 `reverse_lookup(master_id, channel_ref) -> Option<String>`，读同一张 binding 表，必要时加反向 index
- 修改 `crates/zeroclaw-gateway/src/api.rs`：新增 `GET/DELETE /api/approvals/grants*` 端点
- 测试：按 §10 全量补齐
- 文档：更新 `docs/book/src/agents/delegation.md` 或同类 ops 文档说明新行为

### 非本次范围（后续可独立迭代）

- Grant TTL / 自动过期
- 多 daemon 跨进程同步
- 审批委托链（A 把审批权委托给 B）
- 卡片 LLM 摘要在 channel 侧的可视化效果优化（emoji / 颜色 / 折叠）
- Gateway 新增 `GET /api/approvals/audit` 端点（短期内可借既有 `/api/logs` + action 过滤实现等价能力）
- 新增 `EventCategory::Approval` 变体（如未来确实需要把审批与 tool 调用分流，可考虑升 `zeroclaw-log` minor 版加变体）
- 移植到 `zeroclaw-approval` 独立 crate 的重构（方案 B）

## 12. 关联与影响

- **PR #50（approval cards to correct topic thread）**：复用其 `thread_ts` 路由；本设计的 self-approval 场景沿用其行为
- **`AGENTS.md` DRY 章节**：本设计明确不复制 `superusers` / `binding` 状态；新增字段（`granted_by`/`granted_via`）属于「source of truth — created here」
- **稳定性 tier**：`zeroclaw-runtime` 为 Experimental，可做较大调整；`zeroclaw-api::channel` 为 Experimental，新增可选方法不破坏现有实现
