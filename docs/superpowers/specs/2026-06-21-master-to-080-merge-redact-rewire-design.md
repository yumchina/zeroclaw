# master → 0.8.0 合并 · scrubbing 层迁移 — 设计 spec

> 日期：2026-06-21
> 状态：设计已对齐，待 writing-plans
> 上游 PR：[zeroclaw-labs/zeroclaw#7826 — fix(runtime/agent): move credential redaction to the rendering layer](https://github.com/zeroclaw-labs/zeroclaw/pull/7826)（commit `89e24cc3a`）
> 关联：
> - 本地 PR #48 实施 plan [../plans/2026-06-17-url-allowlist-migration.md](../plans/2026-06-17-url-allowlist-migration.md)
> - 上一次 master→0.8.0 合并方案 [../plans/2026-06-05-master-to-080-merge-plan.md](../plans/2026-06-05-master-to-080-merge-plan.md)
> 阅读对象：团队成员（包括没参与 PR #48 的同事）

## 1. 写这份 doc 的原因

本次把 `upstream/master` 合到本地 `0.8.0`，10 个 upstream commit 中有一个 `89e24cc3a` 触及凭证脱敏（credential scrubbing）链路。它和 4 天前刚落地的本地 **PR #48（URL allowlist 迁移）** 在同一份文件、同一片代码上做了**方向相反**的修改 —— upstream 把 scrubbing 从工具执行层「往上搬」到渲染层，PR #48 则在工具执行层「往下扎」加了 allowlist 旁路。

merge 会产生 8 个文件的冲突。其中 7 个是机械的（结构体新增字段并行），1 个 — `tool_execution.rs` — 是真正的架构选择题。team 后续可能会有同事来问：

- 为什么 0.8.0 又改了一次 scrubbing 路径？
- PR #48 加的东西被「撤回」了吗？
- 用户在 Discord/Lark 上看到支付链接被打码的问题修没修？

这份 doc 把整件事的来龙去脉、决定与理由、以及迁移后的新形态写清楚，避免日后翻 git blame 再凑信息。

## 2. PR #48 解决的业务问题（速读）

ZeroClaw 有两条凭证脱敏链路：

1. **`LeakDetector::scan()`** — 频道响应在发出前扫描
2. **`scrub_credentials()`** — 工具输出在传给 LLM / 写日志 / 上报 observer 前打码

两者都用同一套 7 类凭证正则检测，对 `token=`、`api_key=` 等 key=value 串无差别打码。

**现场事故**：用户让 AI 在瑞幸下单，AI 调 `createOrder` 工具，工具返回支付二维码 URL：

```
payOrderQrCodeUrl: https://open.lkcoffee.com/transfer/qrcode?token=hgnD0jgCF63xxxxxxxx
```

`scrub_credentials` 把 `token=hgnD0jgCF63xxxxxxxx` 误判为凭证 → 打码成 `token=hgnD*[REDACTED]` → 用户拿到不完整 URL → 无法扫码支付。

**PR #48 的方案**（mask → detect/scrub → restore）：

- 配置一份 `[security.leak_detector.url_allowlist]`，列出可信域名 + 路径 glob
- 检测前用占位符把白名单 URL 整段「盖住」 → scrub 跑在盖过的文本上 → 还原 URL
- 强保证：即便同一个 token 字符串在文本别处出现并被打码，**白名单 URL 段内的原始内容始终完整保留**

PR #48 的核心机制：

| 组件 | 作用 |
|---|---|
| `[security.leak_detector]` config | 用户配置 sensitivity + url_allowlist |
| `AllowlistRule` + `compile_glob` | 把 glob 编译成 `(domain_re, path_re)` |
| `mask_allowlist_urls` / `restore_allowlist_urls` | 双向占位符替换 |
| `scrub_credentials_with_allowlist(input, rules)` | 简形 `scrub_credentials` 的 allowlist 变体 |
| `scrub_context::TOOL_LOOP_ALLOWLIST` task-local | 把 allowlist 从 orchestrator 注入到下游所有调用点，避免一路传参 |
| `LeakDetector::from_config` | 频道响应链路接入同一套 allowlist |

orchestrator 在每条入站消息开头：

```rust
TOOL_LOOP_ALLOWLIST.scope(scope_value, async move { /* 整段消息处理 */ }).await
```

下游所有需要 scrubbing 的位置只要调用 `scrub_credentials_with_allowlist(x, &current_allowlist())` 即可拿到当前 allowlist。

## 3. upstream `89e24cc3a` 干了什么

`fix(runtime/agent): move credential redaction to the rendering layer (#7826)` 这个 commit 触及 6 个文件 / 198 加 / 34 删，做的事可以一句话概括：

> **把 scrubbing 从「数据路径」搬到「渲染边界」。数据路径上的 tool result 与 HMAC receipt 永远是原始字节。**

旧形态（合并基祖先）：

```
[tool exec] → scrub_credentials(output) → ToolExecutionOutcome.output
                                                     │
                          ┌──────────────────────────┼──────────────────────────┐
                          ↓                          ↓                          ↓
                  LLM 回灌（已打码）         observer 上报（已打码）       HMAC 签名（已打码）
```

新形态（upstream `89e24cc3a` 之后）：

```
[tool exec] → ToolExecutionOutcome.output（原始字节）
                          │
        ┌─────────────────┼─────────────────┬─────────────────┐
        ↓                 ↓                 ↓                 ↓
  LLM 回灌（原始）    HMAC 签名（原始）  渲染边界 scrub:  渲染边界 scrub:
                                       turn/events.rs    turn/post_exec.rs
                                       emit_tool_result  · observer event
                                                         · CLI progress line
```

`turn/redact.rs` 模块文档把规则钉死：

> Credential redaction for the rendering layer (logs, observer events, and
> UI-facing turn events). **This never runs on the data path**: tool results fed
> back to the model and signed by HMAC receipts always carry raw bytes.

新增 3 个调用点（**全部在渲染边界**）：

| 调用点 | 作用 |
|---|---|
| `turn/events.rs::emit_tool_result` | UI / 编辑器收到的 `TurnEvent::ToolResult { output }` 字段打码 |
| `turn/post_exec.rs::record_executed_outcomes` | observer event 的 `output` 与 `error_reason` 字段打码 |
| `turn/post_exec.rs::render_completion_progress` | CLI 完成态进度行 `❌ tool (Ns): <error_reason>` 打码 |

同时移除 `tool_execution.rs` 中原 4 处 `scrub_credentials(...)` 调用 —— 走 raw 路径。

## 4. 两个改动的冲突点

| 维度 | PR #48（本地 HEAD） | upstream `89e24cc3a` |
|---|---|---|
| **方向** | 在工具执行层（数据路径）加 allowlist 旁路 | 把 scrubbing 整个搬离数据路径 |
| **`tool_execution.rs` 4 处** | `scrub_credentials(&reason)` → `scrub_for_tool_output(&reason)`（内部走 `_with_allowlist` + task-local） | 删除整段 scrubbing，直接 raw |
| **HMAC receipt 签的是什么** | 已打码（保留旧行为） | 原始字节（新） |
| **LLM 看到的 tool result** | 已打码（保留旧行为） | 原始字节（新） |
| **UI / observer / CLI 看到的** | 已打码 + allowlist 旁路 | 已打码（在 3 个渲染点） |

PR #48 与 upstream 的**意图本质上不冲突**，但**实现层位选错了**：PR #48 把 allowlist 接入到工具执行层，而 upstream 已经把那一层的 scrubbing 整个搬走。如果直接保留 PR #48 在 tool_execution 的 4 处改动，会让 LLM 也吃到打码内容（回到旧行为，丧失 upstream 改进），同时跟 upstream 在同一文件冲突 → 既要解冲突又要做坏事。

## 5. 关键决定

| # | 决定 | 否决项 / 理由 |
|---|---|---|
| 1 | **tool_execution.rs 4 处冲突全部采用 upstream 版本**（删除 `let scrubbed_reason = …`） | 保留 PR #48 = 丢掉 upstream 的「raw data path」改进 + 把 allowlist 钉在错的层 |
| 2 | **allowlist 接入点从工具执行层迁移到渲染层**（`turn/redact.rs` 已有现成 `_with_allowlist` 变体；3 个渲染点改为调用之） | 在 `turn/redact.rs::scrub_credentials` 内部直接读 task-local（隐式 allowlist），改 1 处 — 否决理由：渲染层 scrub 也有不在 tool loop 内的调用方（例如 CLI 启动时），任何无 scope 调用都会拿 empty allowlist，但同时也让 `scrub_credentials` 函数语义对 caller 不透明（"为什么这里偷偷读了 task-local？"）；显式传入 + 3 处显式 `&current_allowlist()` 更直白 |
| 3 | **PR #48 的 `scrub_context`、`TOOL_LOOP_ALLOWLIST`、`current_allowlist()`、`LeakDetector::from_config`、`allowlist_from_config`、`mask_allowlist_urls`/`restore_allowlist_urls`、orchestrator scope 包裹全部保留** | 这些是 allowlist 机制的核心，与「接入点」无关；丢掉等于回滚整个 PR #48 |
| 4 | **PR #48 在 `loop_.rs:422` 的 `make_query_summary` 修改保留** | 它本来就是渲染面（summary 文本）；auto-merge 没有冲突，自然保留 |
| 5 | **merge commit 只解决冲突**，allowlist 接入渲染层的代码改动作为 follow-up commit 独立提交 | 混在 merge commit 里 = review 时 diff 噪音大、回退困难 |
| 6 | **类型 1（Skill 结构体）7 个文件 / ~14 处冲突全部保留双方字段** | HEAD 的 `enabled: bool` 与 upstream 的 `slash_options: Vec<SkillSlashOption>` 正交，完全可共存；任何取一边都会破坏对应功能 |
| 7 | **PR #48 的两个集成测试（`allowlisted_url_token_survives_tool_output_scrub` / `non_allowlisted_url_token_still_scrubbed`）从 `tool_execution.rs` 模块搬到 `turn/redact.rs` 模块** | 测试对象不再是 `scrub_for_tool_output`（已删），改测 `scrub_credentials_with_allowlist` + scope；语义不变 |

## 6. 迁移后形态（PR #48 在新架构上的新位置）

```
                       orchestrator: TOOL_LOOP_ALLOWLIST.scope(rules, async move {
                                             ...
[tool exec] → ToolExecutionOutcome.output（原始字节）─────┐
                          │                              │
        ┌─────────────────┼─────────────────┬────────────┼────────────┐
        ↓                 ↓                 ↓            │            ↓
  LLM 回灌（原始）    HMAC 签名（原始）  渲染边界 scrub:                渲染边界 scrub:
                                       turn/events.rs                turn/post_exec.rs
                                                                       (2 个调用点)
                                             │
                                             │  scrub_credentials_with_allowlist(
                                             │      x,
                                             │      &current_allowlist(),  ← 读 task-local
                                             │  )
                                             ↓
                                       allowlisted URL token 保留
                                       其他凭证模式照常打码
                                             ...
                                         })  // scope end
```

最终用户路径（"瑞幸支付链接"场景）的还原：

1. AI 调 `createOrder` → 工具返回完整 URL（含 token）
2. ToolExecutionOutcome.output = **原始字节**（含完整 URL）
3. LLM 看到完整 URL → 把它复述给用户（"请扫码支付：https://open.lkcoffee.com/transfer/qrcode?token=hgnD0jgCF63xxxxxxxx"）
4. 回复经 turn/events.rs 发出 → 进入 `scrub_credentials_with_allowlist` → allowlist 命中 → URL 整段保留 ✅
5. observer event / CLI 进度行 / 日志同理，allowlist 命中保留 ✅
6. **HMAC receipt 签的是原始字节**（upstream 顺带改对的一件事 —— 旧实现签打码后字节，验签时拿不到原文重算）

净收益：

- 用户体验问题修复（PR #48 原目标达成）
- 实现更干净 —— allowlist 只在渲染层接入 1 个函数 + 3 个 caller，相比 PR #48 原版的 5 处少了 2 处
- 跟 upstream 同向 —— 后续从 upstream 同步其他 redact 相关变更，几乎都是无冲突

## 7. 非目标（明确不做）

- **不引入新依赖**、**不改 `scrub_credentials_with_allowlist` 函数签名**（已是 `(input, rules)`），**不改 `[security.leak_detector]` config schema**。
- **不动 `LeakDetector::scan()` 的 mask/restore 实现** —— 那条频道响应链路在 PR #48 已经正确接入，本次 merge 不触及。
- **不修复 `Skill.enabled` 在 upstream `slash_options` 中的语义关系**（"disabled skill 还能不能注册 slash command"是个独立的 follow-up 问题，本次 merge 只确保两个字段共存编译通过）。
- **不重排 tool_execution.rs 错误分支的字段顺序** —— 与 PR #48 在该文件相关的 4 处冲突区段以外的部分都接受 upstream 原样。
- **不改 changelog / CHANGELOG.md** —— merge commit 自带的 commit message 已足够；如团队需要 release note，单独 issue 跟踪。

## 8. 风险与回归点

| 风险 | 缓解 |
|---|---|
| 渲染层 3 处改动遗漏一处 | plan Phase B 每处独立 task + 独立 test，跑 `cargo test -p zeroclaw-runtime turn` 全过 |
| PR #48 的 2 个集成测试搬家后语义漂移 | 搬家步骤里**保留断言原文**（"allowlisted token must survive"），只换测试目标函数名 |
| upstream 其它 9 个 commit 引入未察觉的 redact 链路变更 | merge 后跑 `cargo test --workspace`；额外回归 `LeakDetector::scan`、`sanitize_channel_response` 两条用户可见路径 |
| HMAC receipt 签名内容变更（打码 → 原始）的兼容性 | upstream 的改动本身已经接受这个语义变更（commit message 明说 "always carry raw bytes"），本地无额外回归点；如有线上 receipt 验签缓存，单独清理（不在本次 plan 范围） |
| Skill `enabled=false` + upstream slash_options 互动 | 不在本次范围；merge 后由 Skill 维护者单独评估 |

## 9. 验收标准

- `cargo build --workspace`：0 error / 0 warning（与 0.8.0 现有 baseline 相比无回归）
- `cargo test --workspace`：0 fail
- 手动场景回归（Phase B 完成后）：
  - 配置 `[[security.leak_detector.url_allowlist]] domain = "*.lkcoffee.com"`
  - 让 AI 触发一次返回 `payOrderQrCodeUrl` 的工具调用
  - 在 Discord / Lark / CLI 三处分别确认 URL 完整呈现（含 token）
  - 同一会话内打一段假 `api_key="sk-live-xxxxxxxx"`，确认仍被打码
- PR #48 原集成测试搬家后仍 pass
- merge commit 在 `git log --first-parent` 中可读，message 明确说明"接受 upstream `89e24cc3a` 的渲染层方向；allowlist 接入由 follow-up commit 完成"

## 10. 后续 follow-up（不在本次 plan）

- **`LeakDetector` allowlist 在 dispatcher / sub-agent 场景下的 scope 继承**：当 sub-agent 在另一个 tokio task 里启动，`TOOL_LOOP_ALLOWLIST` 不会自动继承，需 orchestrator 显式传播。PR #48 当前覆盖单层 orchestrator，未覆盖 multi-agent fan-out 场景 —— 待 multi-agent runtime 落地后重新评估。
- **`scrub_credentials` 是否应该 deprecate**：rendering 层全部走 `_with_allowlist` 后，简形 `scrub_credentials` 只剩"测试与 caller 没 scope"两类用户，可考虑标 `#[deprecated]` 提示一律走带 allowlist 版本（即便传 `&[]`）。
- **`scrub_credentials_with_allowlist` 命名**：在渲染层全面成为默认 caller 后，可以考虑把它升级成 `scrub_credentials`、把旧的简形改名 `scrub_credentials_no_allowlist` 之类，让默认调用路径自动带 allowlist。本次不做（避免一次 PR 改太多）。

---

**总结一句话**：upstream 把 scrubbing 提升到了渲染层；PR #48 的 allowlist 应该跟着上去，而不是留在已经被搬空的工具执行层。这次 merge 接受 upstream 方向、保留 PR #48 全部机制、把接入点上移 —— 最终 PR #48 想解决的支付链接问题修得更彻底、HMAC 签名也顺带修对。
