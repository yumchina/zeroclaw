# 审批 cancel-on-fanout 完善（后续 PR）

> 日期：2026-06-18
> 状态：问题清单，待后续 PR 实施
> 关联：
> - 上游 spec [持久化按 topic 维度的工具审批授权设计](2026-06-18-persistent-tool-approval-grants-design.md)
> - 上游 plan [Persistent Tool Approval Grants Implementation Plan](../plans/2026-06-18-persistent-tool-approval-grants.md)
> - 触发原因：上游 PR Wave 2 完成 C3（broker 运行时 wiring）后，final whole-branch review 识别出 3 条 cancel 路径正确性问题，本 PR 标注 best-effort 推迟到本 follow-up

## 1. 背景

上游 PR 的"非 superuser 触发 → broker fan-out 给所有 superuser → 先回为准"语义里，对**失败 / 未中选**的卡片有明确的清理诉求：

- broker 一旦确定 winner，应**主动通知**其他 in-flight 的卡片"已被处理"
- 没回应的 superuser 看到的卡片应**视觉上更新**为「已由 {decider} 处理」，否则误以为还在等他点
- 同时 channel 端 `pending_approvals` 的 oneshot sender 应该被释放，避免泄漏

上游 PR 完成了**形式上**的 cancel 路径：

- `Channel::cancel_approval(approval_id, reason)` trait 默认空方法
- `DawnIMChannel::cancel_approval` 实现：从 `pending_approvals` 移除 entry + 写 Note log
- broker `fan_out` 在 winner 确定后对其余 target 调 `cancel_approval`

但 final review 发现这 3 段实现实际上**全部空跑** —— 编译过、Note log 输出过，对用户和 channel 状态没有任何真正影响。本 spec 把 3 个具体问题与各自的"期望行为"列清，作为下个 PR 的入口。

## 2. 问题清单

每个问题分四段：**症状**、**复现路径**、**影响**、**期望行为**。

### 2.1 I1：broker 与 channel 的 `approval_id` 不联通

**症状**

broker 在 `fan_out` 内生成一个 `approval_id`（`Uuid::new_v4().to_string()`），并在 winner 确定后用它调 `ch.cancel_approval(broker_approval_id, ...)`。但每个 channel 实现（dawn_im、lark 等）在 `request_approval` 内**又各自生成一个本地 `approval_id`**，作为其 `pending_approvals: HashMap<String, _>` 的 key。两个 id 完全无关。

**复现路径**

1. 非 superuser 用户在 dawn_im 触发 shell 工具
2. broker 调 `dawn_im.request_approval(...)` → dawn_im 生成 `local_id = uuid-A`，写 `pending_approvals[uuid-A] = sender`、卡片携带 `uuid-A`
3. broker 持有 `broker_id = uuid-B`
4. 假设另一 superuser 先回，broker 跑 cancel 路径：`ch.cancel_approval(uuid-B, ...)`
5. dawn_im 的 `cancel_approval` 执行 `pending_approvals.write().remove(uuid-B)` → **永远不命中**（库里只有 `uuid-A`）
6. Note log 仍照常写，看起来"做了事"，但实际 `pending_approvals` 中 `uuid-A` 的 entry 没释放

**影响**

- cancel 是 no-op，pending_approvals 条目在卡片实际超时前不会消失
- 即便后续 §2.3 的 patch 发送修了，patch 也找不到 recipient（因为 cancel 入口拿不到 `uuid-A`）
- I2 修复（按 recipient 索引）也建立在 broker 和 channel 共享同一个 id 之上

**期望行为**

整条 fan_out 流程内的 `approval_id` 应该是**单一来源**：broker 生成、写进 `ChannelApprovalRequest`、channel 直接复用作 `pending_approvals` 的 key。broker 后续的 `cancel_approval(approval_id, reason)` 调用必定能在 channel 的 pending 表里命中。

老的"channel 自己生成 id"的语义仅保留为兜底（被 broker 之外的旧调用方调用时回退）。

### 2.2 I2：cancel 索引粒度过粗（多 superuser fallback 到同一 channel 时漏 cancel）

**症状**

broker `fan_out` 当前按 channel_ref 维度跟踪 alive_targets：`Vec<(channel_ref, Arc<dyn Channel>)>`。winner 选出后做循环：

```
for (chref, ch) in alive_targets {
    if chref != winning_chref {
        ch.cancel_approval(...)
    }
}
```

这忽略了 recipient — 当多个 superuser 都通过 master_channel 收到卡（`/bind` 未完成的常见配置），它们的 `channel_ref` 是同一个值，过滤 `chref != winning_chref` 把 winner 自己与 loser 一起跳过。

**复现路径**

1. 配置 `channels.superusers = ["u_admin1", "u_admin2"]`，其中 u_admin2 未在触发所在 channel 上 `/bind`
2. broker 反向解析得到 `targets = [("lark.work", "u_admin1_local_uid"), ("master.dawnim", "u_admin2")]`
3. 由于 u_admin2 走 master_channel fallback，假设两人都通过 master_channel：`targets = [("master.dawnim", "u1"), ("master.dawnim", "u2")]`
4. u1 先回 Approve → winning_chref = `"master.dawnim"`
5. cancel 循环 `chref != "master.dawnim"` → 0 命中 → u2 的卡**永不 cancel**

**影响**

- 多管理员的部署上"另一个 admin 也收到的那张卡"永远停在按钮可点状态
- 如果 u2 后来手贱点了 Deny，该 Deny 决定无去处（broker 已 return），仅孤立写 audit log
- 干扰用户体验，模糊"已处理"的 ownership

**期望行为**

cancel 索引精确到 `(channel_ref, recipient)` 二元组。winner 仅指自己那一份；同 channel_ref 下其他 recipient 的卡照样 cancel。

### 2.3 I5：DawnIM `cancel_approval` 构造卡片但不发送

**症状**

`crates/zeroclaw-channels/src/dawn_im/channel.rs` 的实现长这样：

```rust
async fn cancel_approval(&self, approval_id: &str, reason: &str) -> anyhow::Result<()> {
    let _removed = self.pending_approvals.write().await.remove(approval_id);
    ::zeroclaw_log::record!(INFO, ... "dawn_im: cancel_approval invoked (card patch is best-effort)");
    let _ = build_resolved_card(approval_id, reason);  // 构造之后立即丢弃
    Ok(())
}
```

`build_resolved_card` 返回的卡片对象在 `let _ = ...` 处直接 drop，从未送到 channel 的 RPC 出站层。注释自承「to keep the helper used + linted」（防止编译器报 unused 警告）。

**复现路径**

任何 fan_out 进入 cancel 分支的场景：调用发生、Note log 写出、`build_resolved_card` 构造、立即丢弃、用户屏幕上的卡片**没有任何视觉变化**。

**影响**

- 用户没看到「已由 XX 处理」更新 → 误以为系统漏了自己
- spec §3.4 / §8.2 / CHANGELOG 都描述了"卡片更新为已由 XX 处理"的 UX 承诺，实际全部空转
- I1/I2 修好之后这一步反而成为唯一可见的 UX gap

**期望行为**

`cancel_approval` 在被 broker 调到时，**应当在原会话里给原 recipient 推送一份明显的更新**，告诉用户"此请求已由 {decider} 处理"。形式不强求 patch 卡片（如果 DawnIM RPC 没有 patch 能力，发一条新文本消息 / 一张新卡片同样可接受），关键是用户能感知。

同时 `cancel_approval` 必须能在删除 `pending_approvals` 条目时拿到 recipient — 这意味着 `PendingApproval` 至少需要保留 sender + recipient 两块状态。

## 3. 期望最终状态（用户视角与系统视角）

### 用户视角

- **触发者**：与上游 PR 一致，看到正常审批流程
- **审批人 A（winner）**：点击同意/始终允许/拒绝，卡片按当前 UI 反馈"已记录决定"
- **审批人 B（其他 superuser）**：自己的卡片在 A 点完后**几秒内自动更新**为「已由 {A_display_name} 处理 — 同意/拒绝/始终允许」；不需要再点也不会误点

### 系统视角

- broker 生成的 `approval_id` 是该次审批的 **canonical identifier**，贯穿 broker → channel pending key → cancel 路径
- broker `fan_out` 收尾后，每一个非 winner 的 (channel_ref, recipient) 都被 channel 显式 cancel：`pending_approvals` 中其 entry 移除、对应 recipient 收到 UI 更新
- 同一 channel 上的多 recipient 互不干扰
- audit log 仍由 broker 的 `record_decision` 集中产出 winner 的决定；loser 的卡片**不**产生 audit decision（它们没有真正的"决定"，只是被取消）
- cancel 失败（如 RPC 网络错）不影响主流程：best-effort + warn log，broker 已经 return

## 4. 参考解决方案（提交 PR 前请重新评估是否有更优方案）

> 以下仅作为思路引子。PR 实施时请独立 brainstorm，比较多种方案（如 sticky session approach、event sourcing、channel-managed correlation 等），并基于届时的代码现状选取更合适的设计。本节不是 mandate。

### 4.1 参考方向 A：在 `ChannelApprovalRequest` 增 `approval_id` 字段

把 broker 生成的 `approval_id` 通过 trait API 表面传到 channel：

```rust
pub struct ChannelApprovalRequest {
    pub tool_name: String,
    pub arguments_summary: String,
    pub raw_arguments: Option<serde_json::Value>,
    pub thread_ts: Option<String>,
    pub approval_id: Option<String>,   // 新增；channel 优先使用此 id 作 pending key
}
```

trade-off：
- 优点：契约清晰、向后兼容（Option 默认 None，老调用方不受影响）
- 缺点：trait 表面新增字段，影响所有 channel 实现的解读

### 4.2 参考方向 B：broker 在每个 fan-out 分支独立持有 (channel_ref, recipient, approval_id) → channel 的映射

让 broker 不通过 trait 字段而是通过 `pending_correlations: HashMap<broker_id, (chref, recipient)>` 自行追踪。channel 仍生成自己的 id，broker 持有"我发出的卡里 channel 给的 id 是什么"。

trade-off：
- 优点：trait 不动
- 缺点：要求 channel `request_approval` 把自己生成的 id 通过返回值或回调暴露给 broker —— 比 trait 加字段更扭曲

### 4.3 参考方向 C：把 cancel 完全 channel-side 处理

让 channel 在收到 winner 决定时通过 channel 内部 state machine 自动 cancel 其他 pending（broker 完全不调 `cancel_approval`）。

trade-off：
- 优点：完全去 broker → channel 协调
- 缺点：需要 channel 知道"哪些 pending 属于同一审批批次"，对 channel 抽象侵入大；fan_out 是 broker 维度概念，下放给 channel 不合适

### 4.4 cancel UX 发送形式选择

如果走方向 A，dawn_im 的 cancel 发送方式可参考：

- **patch 卡片**：若协议有 PATCH-equivalent RPC，更新原卡 inline
- **新消息回复**：若没有 patch，发一条新卡或新文本，用户能在会话中读到
- **混合**：能 patch 则 patch，否则 send 新消息；都失败仅 log warn

lark 已具备 `build_resolved_approval_card` patch 能力，复用即可。

### 4.5 测试方向

- broker 单测：fake channel 截 `ChannelApprovalRequest` 验证 `approval_id` 来自 broker；记录 cancel_count 验证按 `(chref, recipient)` 命中
- dawn_im 单测：mock RPC 通道验证 cancel 真正 send 了 resolved 内容
- 组件测试：在内存 fake channel 上跑端到端"两个 superuser + 一个先回 + 另一个收到更新"

具体测试用例数与命名 PR 实施时再定。

## 5. 范围与影响（参考）

按方向 A 估算的改动面（PR 实施时重新核对）：

- `crates/zeroclaw-api/src/channel.rs` — `ChannelApprovalRequest` 新增 `approval_id: Option<String>`
- `crates/zeroclaw-runtime/src/approval/broker.rs` — `fan_out` 调整 alive_targets 跟踪 + 复用 broker `approval_id`
- `crates/zeroclaw-channels/src/dawn_im/channel.rs` — `PendingApproval` 保留 recipient + `cancel_approval` 真发卡
- `crates/zeroclaw-channels/src/dawn_im/approval.rs` — `PendingApprovals` 类型别名调整
- `crates/zeroclaw-channels/src/lark.rs` — 同模式（lark 已有 patch 能力可复用）
- 测试新增/扩展
- `CHANGELOG-next.md` — 去掉上游 PR 的"cancel-on-fanout best-effort"caveat

任何其他方案的范围请实施 PR 时重新评估。

## 6. 实施前请确认

下个 PR 启动 brainstorming / writing-plans 时，请回答：

1. 上述 3 个问题在 PR 实施时是否仍然存在？（grep 验证）
2. 参考方向 A/B/C 之外有没有更优解？（特别留意届时的 trait 演进 / 新 channel 添加 / wasm plugin 边界）
3. cancel 的 UX 发送形式（patch / 新消息 / 混合）哪个对你的 superuser 群体更友好？
4. 哪些 channel 需要同步适配？（dawn_im、lark 已知；slack/telegram/discord 如果届时已实现 `request_approval` 也要考虑）

确认后再进入 writing-plans。
