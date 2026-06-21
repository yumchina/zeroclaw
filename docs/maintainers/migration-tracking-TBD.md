# Migration Tracking — yumchina/master PR #34–#53 → 0.8.0

> 生成日期：2026-06-13（#34–#46）；2026-06-17 追加 #47–#50；2026-06-21 追加 #51–#53
> 范围：yumchina/zeroclaw master 分支 PR #34 至 #53
> 目的：记录各 PR 在 0.8.0 分支的迁移状态、优先级及迁移建议

## 状态图例

- ✅ **已迁移** — 功能完整移植到 0.8.0
- 🔶 **部分迁移** — 核心功能已迁移，仍有遗留内容
- ❌ **未迁移** — 尚未在 0.8.0 中实现
- ⏭️ **不适用** — 已撤销、被替代方案覆盖或与 0.8.0 架构不兼容无需移植

## 优先级图例

- **P1** — 立即处理（影响范围广、修复成本低）
- **P2** — 尽快处理（安全/稳定性问题）
- **P3** — 计划内（功能完整性，需架构适配）
- **P4** — 跟进处理（依赖 P 级前置项）
- **P5** — 长期（工作量大，需专项排期）
- **P6** — 待评估（架构差异大，需重新设计）
- **—** — 无需排期

---

## 迁移明细

| PR | 标题 | 状态 | 优先级 | 迁移建议 | 最终结论 |
|----|------|------|--------|---------|---------|
| **#34** | 任务超时异常处理，优化审批流中断服务 | 🔶 部分迁移 | P5 | Error-card Phase 1（ERR 码渲染 + 异常卡片）已迁移；Retry/Intervene/Cancel 人工接管交互流程未实现，工作量最大，需专项排期 | 已经完成，其余放弃 |
| **#35** | 优化toolid，修复异常未回复 | ❌ 未迁移 | P6 | 依赖旧 `zeroclaw-channel-wukongim` crate 和 `zeroclaw-progress-observer`，均已在 0.8.0 重构/消除；需重新评估是否在 0.8.0 新架构上以新方式实现 | 暂缓 — 待 0.8.0 dawn_im 架构稳定后重新评估原 bug 是否仍存在 |
| **#36** | fix(runtime): ping-pong loop detector false-positive | ✅ 已迁移 | — | 已 cherry-pick 至 0.8.0，对应提交 `efb74568` | 迁移完成（cherry-pick `efb74568`） |
| **#37** | fix: implement safe task-local access using try_with | ⏭️ 不适用 | — | 依附于 `CURRENT_TOOL_*` task-local 机制（#43 已撤销），0.8.0 progress observer 走不同路径，此补丁不需要 | 放弃 |
| **#38** | fix: force PowerShell UTF-8 output encoding & dawn_s3 enhancements | 🔶 部分迁移 | — | dawn_s3 核心功能已通过 Area B 迁移覆盖；PowerShell PYTHONIOENCODING 部分已跳过，0.8.0 用 `encoding_rs` + GBK 转码替代，方案更通用 | dawn_s3 增强部分已迁移完成；PowerShell PYTHONIOENCODING 放弃（被 0.8.0 `encoding_rs` + GBK 通用方案取代） |
| **#39** | feat: xuanji-Dawn 文档提取集成 via WuKongIM bridge | ✅ 已迁移 | P3 | 依赖旧 `zeroclaw-channel-wukongim` crate 和 `XUANJI_BRIDGE` mpsc 机制；需与 #46 捆绑，按 0.8.0 的 `dawn_im` 模块架构重写 | 已迁移完成（与 #46 捆绑实施）。**后续通过 [dawn-tools 与 channel 解耦设计](../superpowers/specs/2026-06-14-dawn-tools-channel-decoupling-design.md) + [实施计划](../superpowers/plans/2026-06-14-dawn-tools-channel-decoupling.md) 演化为 `PerToolChannelHandle` + `SendKind::TaskSubmit/Query` 模式**，消除全局 mpsc bridge 和 `zeroclaw-channels → dawn-tools` 反向依赖 |
| **#40** | fix(agent): 最终回复不再拼接中间迭代的英文叙述文本 | ✅ 已迁移 | P1 | 一行修复（`loop_.rs` 末次迭代 `push_str` → 赋值替换），成本极低，直接影响中文回复质量，优先级最高 | 迁移完成。注：upstream #7540 agent loop refactor 反掉过一次，2026-06-14 merge 时在新位置 `agent/turn/mod.rs:555` 重新 apply |
| **#41** | fix(security): seatbelt getcwd traversal permission denied | ✅ 已迁移 | P2 | `seatbelt.rs` 已存在于 0.8.0，仅需移植 getcwd traversal 修复；属安全/稳定性问题，macOS 用户有明显感知 | 迁移完成 |
| **#42** | Implement macOS Seatbelt wildcard port mapping | 🔶 部分迁移 | P4 | `seatbelt.rs` 无 wildcard port 逻辑；该 PR 同时涉及旧 wukongim crate 的 approval/filter，需在 #41 完成后评估移植范围 | seatbelt 相关部分已迁移（`network_outbound_allow` 配置字段 + schema.rs），wukongim approval/filter 部分放弃 |
| **#43** | Add environment info logging before command execution | ⏭️ 已撤销 | — | 被 #44 revert，功能已撤销，无需迁移 | 放弃 |
| **#44** | Revert "feat(shell): 增加命令执行前的环境信息记录" | ⏭️ 已撤销 | — | revert 本身无功能内容 | 放弃 |
| **#45** | Implement multi-topic mapping logic and refactor settings field | ✅ 已迁移 | P3 | 修改了旧 `zeroclaw-channel-wukongim` crate；需重新评估是否在 0.8.0 上以新方式实现，或待 dawn_im 架构稳定后统一处理 | 已通过 [DawnIM 多话题映射 thread 设计](../superpowers/specs/2026-06-14-dawn-im-multi-topic-design.md) + [实施计划](../superpowers/plans/2026-06-14-dawn-im-multi-topic.md)（方案 ζ）重新设计完成。**不照搬原 PR**：跳过 SettingFlags 重构；新增 `ChannelOrigin.topic` 暴露给工具栈；offline batch 按 topic 拆分；继续用 `setting: Option<u32>` + `Some(8u32)` |
| **#46** | refactor: rename xuanji tools to generic dawn task tools | ✅ 已迁移 | P3 | `dawn_task.rs` / `dawn_agents.rs` 在 0.8.0 均不存在；需与 #39 捆绑，按 0.8.0 的 `dawn_im` + `dawn-tools` crate 架构重写 | 已迁移完成。`dawn_task.rs`（任务类型配置）进 zeroclaw-config；`task.rs`（工具实现 `CreateTaskTool` / `QueryTaskTool`）进 dawn-tools crate；新增 `ToolKind::DawnTask` 归属。**后续通过 [解耦设计](../superpowers/specs/2026-06-14-dawn-tools-channel-decoupling-design.md) + [实施计划](../superpowers/plans/2026-06-14-dawn-tools-channel-decoupling.md) 把配置项扩展为 `DawnTaskExecutorConfig` (channel + recipient)，允许任意 channel 充当 task executor** |
| **#47** | feat(wukongim): sequential watermark commit + background recovery | ❌ 未迁移 | P6 | 主体在 `zeroclaw-channel-wukongim/src/channel.rs`（+340 行），0.8.0 已无此 crate，watermark/恢复机制需在 `dawn_im` 上重写；`orchestrator/mod.rs` 中 `followup_thread_id` 对 `wukongim` 特判 0.8.0 也未覆盖 `dawn_im`（root message 仍可能用 `msg.id` 当 thread） | 待 `dawn_im` 历史同步策略设计稳定后重新评估；先把 `followup_thread_id` 对 `dawn_im` 的特判迁过来止血 |
| **#48** | feat(security): 为泄露检测器添加可配置 URL 白名单 | ✅ 已迁移 | P2 | 改 `runtime/src/security/leak_detector.rs`（+352 行）+ `security/mod.rs` + `agent/tool_execution.rs`；0.8.0 同名文件存在且体积接近（22.3K），但 grep `allowlist/allow_list/allowed_url` 均无命中。改动与 wukongim 解耦，可按 0.8.0 现有 security 架构直接移植，附 spec `docs/superpowers/specs/2026-06-12-url-allowlist-design.md` 可循 | 迁移完成。适配 0.8.0 现状:`scrub_credentials` 调用点 5 处(vs master 1 处)全部改造;新增 `crates/zeroclaw-runtime/src/agent/scrub_context.rs` 承载 `TOOL_LOOP_ALLOWLIST` task-local;`sanitize_channel_response` 加 `&LeakDetectorConfig` 参数;两处 `LeakDetector::new()` 切换为 `from_config`,scope 注入在 `process_channel_message_body`。配置入口 `[security.leak_detector]`,默认空白名单,行为完全向后兼容。详见 [实施计划](../superpowers/plans/2026-06-17-url-allowlist-migration.md)。 |
| **#49** | fix: 修复 macOS 休眠唤醒后 WuKongIM 客户端连接死锁 | ⏭️ 不适用 | — | 改动仅在 `zeroclaw-channel-wukongim/src/channel.rs`，0.8.0 无此 crate | dawn_im 不存在同类死锁：`ws_sink` RwLock 仅短暂持有（无嵌套循环等锁）；`send_rpc` 含 30s oneshot 超时；listen 循环含 90s 心跳超时 + `read.next()` 错误自动退出；`process_inbound_message` 以 `tokio::spawn` 隔离（INVARIANT 注释已明确禁止 inline await）。休眠唤醒后 TCP 静默失效最多触发重连，不会死锁，无需移植。 |
| **#50** | Fix message routing for approval and intervention in threads | ✅ 已迁移 | P3 | 改 `zeroclaw-api/src/channel.rs`（`ChannelApprovalRequest` 加字段）+ `acp_channel.rs` + `orchestrator/mod.rs` + `agent.rs` + `loop_.rs` + `approval/mod.rs` + wukongim `approval/card.rs`。0.8.0 上 `ChannelApprovalRequest` 仍只有 `tool_name/arguments_summary/raw_arguments`，`dawn_im::channel.rs::request_approval` 调用 SendParams 时 `topic: None`，approval/接管卡片不会落到对应 thread。#45 的多话题 transparent 透传只覆盖普通 SendMessage 路径，approval/intervention 路径需要补 thread_ts/topic 字段 | 迁移完成。策略：复用 `CHANNEL_ORIGIN.topic` task-local（已由 #45 建立），无需改动 `run_tool_call_loop` 签名。`ChannelApprovalRequest` 新增 `thread_ts: Option<String>` + `Default` derive；`approval_gate.rs` 通过 `CHANNEL_ORIGIN.try_with` 读取 topic 并填入；`dawn_im::channel.rs::request_approval` 调用 `topic_to_thread` 将 `thread_ts` 映射为 `SendParams.topic`（过滤空串哨兵）+ `setting: Some(8u32)`。所有构造点（acp_channel.rs 8 处、approval/mod.rs 1 处、rpc/approval_channel.rs 3 处、dawn_im/approval.rs）补全字段，workspace 编译 0 错误。 |
| **#51** | Improve status update handling and clean up sender history（含 2 commits：sentinel 清理 + status_update fire-and-forget） | 🔶 部分迁移 | P2 | **Commit 1 `be488c1cf`（channels: strip sentinel assistant turns）**：0.8.0 `orchestrator/mod.rs` 仍保留三种 sentinel 字符串（"Task failed/timed out/Session interrupted — not continuing this request"，行 5929/5979/10116）写入路径，但**未做读出端清理**。`prior_turns` 构造点位于行 4743（`normalize_cached_channel_turns` 之后），新清理块可直接插入在 image 剥离（行 4765）之前。改动与 channel 无关，影响所有 dawn_im / acp / 其他 channel，属 history poisoning 止血修复，成本低（+~30 行 + 单元测试）。**Commit 2 `8d5ae4e23`（wukongim: status_update fire-and-forget）**：0.8.0 `Channel` trait 无 `send_status_update` 方法；`DawnIMConfig.progress_streaming` 字段在 `schema.rs:12847` 注释明确 "Currently unwired in the 0.8.0 port — reserved for future re-implementation"。状态流式上报整条链路尚未接回，fire-and-forget 策略需在重接时一并设计 | **Commit 1 待迁移**：直接移植 SENTINELS 数组 + while 循环到 `prior_turns` 处理段，附原 PR 的语义（删除 sentinel 同时回收前置 user turn 保证 user/assistant 交替）。**Commit 2 暂缓**：等 0.8.0 status streaming surface 重新接入时一并采纳 fire-and-forget 设计 |
| **#52** | Refactor outbound message handling and improve heartbeat detection（含 2 commits） | ❌ 未迁移 | P2 | **Commit 1 `a933837a5`（tighten heartbeat detection）**：0.8.0 `dawn_im/channel.rs` 心跳循环（行 1486–1503）存在与 master wukongim 同款 bug —— `last_activity = Instant::now()`（行 1506）在 `let WsMsg::Text(text) = frame else { continue; };`（行 1507）**之前**执行，导致 TCP 控制帧（Ping/Pong/Binary）刷新活跃度，server 静默时无法及时检测出僵尸连接。原 PR 还移除了 `send_ws_frame(WsMsg::Ping)` —— 0.8.0 dawn_im 心跳循环（行 1483+）只发 JSON-RPC ping，**无需该项移植**。HEARTBEAT_TIMEOUT 90→60 适用于 `dawn_im/connection.rs:12`（但被 #53 进一步收紧到 25s，最终值取 #53）。**Commit 2 `4aecb777c`（replace send_rpc("send") with fire-and-forget）**：0.8.0 `dawn_im/channel.rs` 5 处沿用 `send_rpc("send", params)` 等 30s ACK 的反模式（行 853, 951, 1354, 1598, 1636）。WuKongIM/Dawn 服务器实际不可靠回 send ACK，导致 approval 卡片在用户响应前就 RPC timeout（与 #50 thread routing 修复呼应）。需提取 `send_params_fire_and_forget` 复用相同直发 WS 帧路径 | **Commit 1 部分迁移**：移动 `last_activity = Instant::now();` 到 `let WsMsg::Text(...) else { continue; };` 之后（结构性 bug 修复），数值调整跟 #53。**Commit 2 待迁移**：dawn_im 5 处 `send_rpc("send", ...)` 切换为 `send_params_fire_and_forget`，注意 `request_approval`/`request_intervention` 已绑定 `pending_approvals` oneshot 等待用户响应，仅 RPC 帧本身改为不等 ACK 即可 |
| **#53** | Enhance WebSocket stability with TCP keepalive and heartbeat adjustments | ❌ 未迁移 | P2 | 4 项独立改动：（a）**TCP keepalive via socket2**（10s idle / 5s interval）—— 0.8.0 `dawn_im/channel.rs:1377-1382` 用 `tokio_tungstenite::connect_async` 直连，需重写为先用 `socket2::Socket` 设置 keepalive 再 `client_async`，并把 `socket2` 加入 `zeroclaw-channels` 依赖。（b）**PING_INTERVAL 30→10 + HEARTBEAT_TIMEOUT 60→25** 适用于 `dawn_im/connection.rs:11-12`，与 #52#1 配套生效。（c）**Backoff 稳定运行重置** 适用于 `orchestrator/mod.rs:4170+` `spawn_supervised_listener_with_health_interval`：0.8.0 当前 `Err(_)` 分支不重置 backoff（行 4250–4261），导致长跑后掉线时仍需等指数退避。在 `let result = { ... };`（行 4200–4213）之前记 `start_time`，在 `match result { ... }` 之前判断 `start_time.elapsed() >= 30s` 时重置 `backoff = initial_backoff_secs.max(1)`。（d）**LocalTimer with microsecond precision** —— master 改在 `src/main.rs::init_logging`；0.8.0 已将 tracing 配置统一搬至 `crates/zeroclaw-log/src/subscriber.rs`，`AgentAliasFormatter::inner` 当前持有 `fmt::format::Format<Full, fmt::time::SystemTime>`（行 102），把 `SystemTime` 替换为基于 `chrono::Local` 的自定义 `FormatTime` 即可。需考虑 `zeroclaw-log` 是否已依赖 `chrono` | 待整体迁移：4 项均与 channel 解耦或可干净适配，作为 dawn_im 网络稳定性合并补丁推进，建议与 #52#1（last_activity 修复）一起出 PR |

---

## 推荐执行顺序

1. **P1 — #40**：一行修复，立即处理。`loop_.rs` 末次迭代改为赋值替换。
2. **P2 — #41 / #48 / #51 / #52 / #53**：
   - #41 seatbelt getcwd traversal 安全修复，`seatbelt.rs` 直接移植；
   - #48 URL 白名单已迁移；
   - **#51 commit 1**（sentinel 清理）独立成 PR，修复 history poisoning 引发的"[Task failed ...]"回声 bug，与 channel 无关，成本低优先做；
   - **#52 commit 1（last_activity 结构性修复）+ #52 commit 2（send_rpc → fire-and-forget）+ #53（TCP keepalive + 心跳收紧 + backoff 重置 + LocalTimer）** 建议作为 "dawn_im 网络稳定性合并补丁" 一次性出 PR，避免心跳数值/调度顺序之间留下中间态。
3. **P3 — #39 + #46 / #50**：dawn task tools 捆绑实施（已完成）；#50 approval/intervention thread routing 已完成（扩展 `ChannelApprovalRequest` + 联通 dawn_im）。
4. **P4 — #42**：seatbelt wildcard port mapping，#41 完成后跟进。
5. **P5 — #34**：human takeover 完整实现（Retry/Intervene/Cancel），工作量最大，需专项排期。
6. **P6 — #35 / #47 / #51 commit 2**：依赖旧 wukongim crate 或尚未接回 0.8.0 的子系统，架构差异大；#49 已确认不适用（dawn_im 无同类死锁）；#35/#47 待 `dawn_im` 架构稳定后重新评估是否需要等效实现；**#51 commit 2（status_update fire-and-forget）** 待 0.8.0 status streaming surface（`progress_streaming` 配置项目前 unwired）重新接入时一并采纳。
