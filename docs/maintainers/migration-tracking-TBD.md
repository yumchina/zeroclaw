# Migration Tracking — yumchina/master PR #34–#46 → 0.8.0

> 生成日期：2026-06-13
> 范围：yumchina/zeroclaw master 分支 PR #34 至 #46
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
| **#35** | 优化toolid，修复异常未回复 | ❌ 未迁移 | P6 | 依赖旧 `zeroclaw-channel-wukongim` crate 和 `zeroclaw-progress-observer`，均已在 0.8.0 重构/消除；需重新评估是否在 0.8.0 新架构上以新方式实现 | 暂时放弃 |
| **#36** | fix(runtime): ping-pong loop detector false-positive | ✅ 已迁移 | — | 已 cherry-pick 至 0.8.0，对应提交 `efb74568` |  |
| **#37** | fix: implement safe task-local access using try_with | ⏭️ 不适用 | — | 依附于 `CURRENT_TOOL_*` task-local 机制（#43 已撤销），0.8.0 progress observer 走不同路径，此补丁不需要 | 放弃 |
| **#38** | fix: force PowerShell UTF-8 output encoding & dawn_s3 enhancements | 🔶 部分迁移 | — | dawn_s3 核心功能已通过 Area B 迁移覆盖；PowerShell PYTHONIOENCODING 部分已跳过，0.8.0 用 `encoding_rs` + GBK 转码替代，方案更通用 | 放弃 |
| **#39** | feat: xuanji-Dawn 文档提取集成 via WuKongIM bridge | ✅ 已迁移 | P3 | 依赖旧 `zeroclaw-channel-wukongim` crate 和 `XUANJI_BRIDGE` mpsc 机制；需与 #46 捆绑，按 0.8.0 的 `dawn_im` 模块架构重写 | 已迁移完成（与 #46 捆绑实施）。**后续通过 [dawn-tools 与 channel 解耦设计](../superpowers/specs/2026-06-14-dawn-tools-channel-decoupling-design.md) + [实施计划](../superpowers/plans/2026-06-14-dawn-tools-channel-decoupling.md) 演化为 `PerToolChannelHandle` + `SendKind::TaskSubmit/Query` 模式**，消除全局 mpsc bridge 和 `zeroclaw-channels → dawn-tools` 反向依赖 |
| **#40** | fix(agent): 最终回复不再拼接中间迭代的英文叙述文本 | ✅ 已迁移 | P1 | 一行修复（`loop_.rs` 末次迭代 `push_str` → 赋值替换），成本极低，直接影响中文回复质量，优先级最高 | 迁移完成 |
| **#41** | fix(security): seatbelt getcwd traversal permission denied | ✅ 已迁移 | P2 | `seatbelt.rs` 已存在于 0.8.0，仅需移植 getcwd traversal 修复；属安全/稳定性问题，macOS 用户有明显感知 | 迁移完成 |
| **#42** | Implement macOS Seatbelt wildcard port mapping | 🔶 部分迁移 | P4 | `seatbelt.rs` 无 wildcard port 逻辑；该 PR 同时涉及旧 wukongim crate 的 approval/filter，需在 #41 完成后评估移植范围 | seatbelt 相关部分已迁移（`network_outbound_allow` 配置字段 + schema.rs），wukongim approval/filter 部分放弃 |
| **#43** | Add environment info logging before command execution | ⏭️ 已撤销 | — | 被 #44 revert，功能已撤销，无需迁移 | 放弃 |
| **#44** | Revert "feat(shell): 增加命令执行前的环境信息记录" | ⏭️ 已撤销 | — | revert 本身无功能内容 | 放弃 |
| **#45** | Implement multi-topic mapping logic and refactor settings field | ❌ 未迁移 | P6 | 修改了旧 `zeroclaw-channel-wukongim` crate；需重新评估是否在 0.8.0 上以新方式实现，或待 dawn_im 架构稳定后统一处理 |  |
| **#46** | refactor: rename xuanji tools to generic dawn task tools | ✅ 已迁移 | P3 | `dawn_task.rs` / `dawn_agents.rs` 在 0.8.0 均不存在；需与 #39 捆绑，按 0.8.0 的 `dawn_im` + `dawn-tools` crate 架构重写 | 已迁移完成。`dawn_task.rs`（任务类型配置）进 zeroclaw-config；`task.rs`（工具实现 `CreateTaskTool` / `QueryTaskTool`）进 dawn-tools crate；新增 `ToolKind::DawnTask` 归属。**后续通过 [解耦设计](../superpowers/specs/2026-06-14-dawn-tools-channel-decoupling-design.md) + [实施计划](../superpowers/plans/2026-06-14-dawn-tools-channel-decoupling.md) 把配置项扩展为 `DawnTaskExecutorConfig` (channel + recipient)，允许任意 channel 充当 task executor** |

---

## 推荐执行顺序

1. **P1 — #40**：一行修复，立即处理。`loop_.rs` 末次迭代改为赋值替换。
2. **P2 — #41**：seatbelt getcwd traversal 安全修复，`seatbelt.rs` 直接移植。
3. **P3 — #39 + #46**：xuanji/dawn task tools 完整功能，捆绑处理，按 `dawn_im` + `dawn-tools` 架构重写。
4. **P4 — #42**：seatbelt wildcard port mapping，#41 完成后跟进。
5. **P5 — #34**：human takeover 完整实现（Retry/Intervene/Cancel），工作量最大，需专项排期。
6. **P6 — #35 / #45**：依赖旧架构，架构差异大，需重新评估是否以新方式实现，或纳入后续版本规划。
