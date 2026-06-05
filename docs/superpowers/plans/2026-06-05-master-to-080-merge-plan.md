# master → 0.8.0 合并方案

> 编写日期：2026-06-05
> 复核状态：✅ 已通过实测验证（`git cherry-pick -n`、`git grep`、`git diff`）

## 1. 背景

master 分支于 2026-06-02 ~ 2026-06-04 期间新增了 6 个待合并 commit（提交区间
`b897c0f2c..f62510755`）。需评估如何将其中有价值的功能合并进 0.8.0 分支。

### 分支现状

两分支从共同祖先 `233c0115`（数月前）分叉后：

- **master**：分叉点后 50 个 commit
- **0.8.0**：分叉点后 **603 个 commit**，架构已大幅演进

### 0.8.0 相对 master 的关键架构差异

| 方面 | master | 0.8.0 |
|------|--------|-------|
| WuKongIM | 独立 crate `zeroclaw-channel-wukongim/` | 已并入 `zeroclaw-channels/src/dawn_im/` |
| Progress Observer | 独立 crate `zeroclaw-progress-observer/` | 已并入 `orchestrator/progress.rs`（文本-only） |
| Task-local 变量 | `CURRENT_TOOL_CALL_ID` + `CURRENT_TOOL_NAME`（定义于 `zeroclaw-api/src/lib.rs`） | **已删除**，替换为 `NATIVE_THINKING_OVERRIDE` |
| Windows 中文编码 | 注入 `PYTHONIOENCODING=utf-8` 环境变量 | 已实现 `windows_code_page_to_encoding()` + `encoding_rs` GBK 转码 |
| Orchestrator | 较小 | 已大幅重写（约 11773 行差异） |

---

## 2. 待合并 Commit 介绍

### `9e769b894` — refactor: WuKongIM 代码格式化 + 离线批量过滤设计文档

纯重构 + 文档。对 `channel.rs`（450 行变动）格式整理，并新增三份设计文档
（update-from-server、wukongim-offline-batch-filter 等）。

### `e816fb804` — fix: 优化任务超时的问题（Human Takeover 人工介入）

本批最大功能 commit。解决「步骤超时或死循环时智能体直接报错退出」的问题。

- **新增 API 类型**（`zeroclaw-api/src/channel.rs`）：
  ```rust
  pub struct ChannelInterventionRequest {
      pub reason: String,        // "步骤超时" / "死循环检测"
      pub last_tool: Option<String>,
      pub error_detail: String,
  }
  pub enum ChannelInterventionResponse { Retry, Cancel, Intervene }
  ```
- **新增 trait 方法** `Channel::request_intervention()`（默认返回 `Ok(None)`）。
- **Orchestrator 新流程**：执行错误/超时时调用 `request_intervention()` 发送干预卡片，
  用户可选 **Retry**（重试步骤）/ **Intervene**（直接输入新指令）/ **Cancel**（取消）。
  新增全局 `suspended_tasks` HashMap，路由挂起任务期间用户发来的消息。
- 注意：master 将 orchestrator 中原有的 **model switch 处理点直接替换** 为 intervention 处理。

### `45c3179a4` — feat: 工具执行进度指示（task-locals）

让用户在 WuKongIM 聊天界面实时看到智能体当前执行的工具及耗时。

- `zeroclaw-api/src/lib.rs` 新增两个 task-local：`CURRENT_TOOL_CALL_ID`、`CURRENT_TOOL_NAME`。
- `tool_execution.rs`（及 `agent.rs`）将工具执行包裹在 `.scope()` 中写入这两个变量。
- WuKongIM `channel.rs` 在发送 STATUS_UPDATE 时读取 task-local，在 JSON payload 附加
  `tool_name` / `tool_call_id` / `success` / `elapsed_ms`。

### `5e3a89048` — fix: 乒乓循环检测器误报修复

`LoopDetector` 的 Pattern 2（乒乓检测）原先只看工具名，导致「写代码 → 运行 → 修复 → 运行」
这类正常工作模式被误判为死循环并强制 Break。

修复：新增 `args_hash` + `result_hash` 双重静止性检测——只有两个工具的**参数与结果都完全不变**
才判定为真死循环（维持 Block/Break 升级）；任一有变化则仅在 6+ 循环时发 Warning。
同时修复 `suggestions.rs` 中一处 `Skill` 字面量缺失 `enabled` 字段的测试编译错误。

### `30bbfb15d` — fix: Windows 子进程强制 PYTHONIOENCODING=utf-8

Windows 下 Python 默认 stdout 编码为 GBK/cp936，导致 `dawn-xlsx` 等 skill 运行 Python
脚本时中文乱码。修复：在 `shell.rs` 和 `skill_tool.rs` 中对每个子进程显式注入
`PYTHONIOENCODING=utf-8`。

### `bf0d30e65` — fix: task-local 安全访问（try_with 防 panic）

`45c3179a4` 引入的 task-local，若在工具执行 `scope` 之外调用 `.with()` 会 panic。
修复：将 `.with(Clone::clone)` 改为 `.try_with(Clone::clone).ok().flatten()`，scope 外返回
`None` 而非 panic。**本质是 `45c3179a4` 的依附补丁。**

---

## 3. 合并评估（已实测验证）

| Commit | 处置 | 依据（已验证） |
|--------|------|--------------|
| `5e3a89048` | ✅ **直接 cherry-pick** | 实测 `git cherry-pick -n` 返回 exit 0 无冲突。loop_detector.rs 与 master 修改前逐字节一致；suggestions.rs 两边都加了 `enabled` 字段，git 三方合并自动去重，无重复字段编译错误 |
| `45c3179a4` | 🔨 **手动移植** | 依赖的 `CURRENT_TOOL_*` task-local 在 0.8.0 已删除；WuKongIM 已重构为 dawn_im |
| `bf0d30e65` | 🔨 **并入 `45c3179a4` 一起移植** | 0.8.0 的 `progress.rs` 完全不用 task-local，panic 点不存在；它纯粹依附于 `45c3179a4` |
| `e816fb804` | 🔨 **手动移植（注意并存）** | 0.8.0 orchestrator 无 intervention 代码，但仍保留 model switch 逻辑（`is_model_switch_requested`、`/model` 等命令仍在用）|
| `30bbfb15d` | ⏭️ **跳过** | 0.8.0 无 PYTHONIOENCODING，但已用 `windows_code_page_to_encoding()` + `encoding_rs` GBK 转码从根本解决，机制更通用（不限 Python）|
| `9e769b894` | ⏭️ **跳过** | WuKongIM 已重构为 dawn_im，格式化无移植价值；设计文档可选择性存档 |

---

## 4. 执行方案

### 第 1 步：直接 cherry-pick（已实测可行）

```bash
git cherry-pick 5e3a89048   # loop detector 误报修复，exit 0 无冲突
```

### 第 2 步：移植 tool progress indicators（`45c3179a4` + `bf0d30e65` 捆绑）

1. `zeroclaw-api/src/lib.rs`：在 `tokio::task_local!` 中加回 `CURRENT_TOOL_CALL_ID`、
   `CURRENT_TOOL_NAME`（与现有 `NATIVE_THINKING_OVERRIDE` 并存）。
2. `zeroclaw-runtime/src/agent/tool_execution.rs` **及 `agent.rs`**：将工具执行包裹进
   `.scope()` 写入 task-local。⚠️ 别漏 `agent.rs`（master `agent.rs:1048` 也用了）。
3. `zeroclaw-channels/src/dawn_im/channel.rs`：在 status update 发送处读取 task-local，
   附加 `tool_name` / `tool_call_id` / `success` / `elapsed_ms` 到 payload。
4. **读取处直接采用 `try_with(...).ok().flatten()` 最终形态**（即合入 `bf0d30e65`），
   不要分两步先写 `.with()` 再改。

### 第 3 步：移植 human takeover（`e816fb804`）

1. `zeroclaw-api/src/channel.rs`：复制 `ChannelInterventionRequest` /
   `ChannelInterventionResponse` 结构体及 `request_intervention()` trait 方法（冲突小）。
2. `zeroclaw-channels/src/dawn_im/approval.rs`：参考 master 的
   `approval/card.rs` + `approval/mod.rs`，移植介入卡片构建逻辑。
3. `zeroclaw-channels/src/orchestrator/mod.rs`：在超时/错误处理点织入 `request_intervention`
   调用与 `suspended_tasks` 路由。
   ⚠️ **关键**：0.8.0 仍保留 model switch 逻辑，intervention 必须作为**新增分支与之并存**，
   不能照搬 master 那样替换 model switch，否则会破坏 0.8.0 现有的 `/model` 运行时切换。

### 跳过项

- `30bbfb15d`：GBK 转码已覆盖。
- `9e769b894`：WuKongIM 已重构。

---

## 5. 风险与注意事项

- **唯一可直接 cherry-pick 的只有 `5e3a89048`**，其余有价值功能均需手动适配 0.8.0 的
  dawn_im + 新 orchestrator 结构。
- `45c3179a4` 与 `bf0d30e65` 必须**捆绑**移植，避免引入已知 panic。
- `e816fb804` 移植的最大风险点是 orchestrator 的 model switch 并存问题，移植后需回归测试
  `/model`、`/models`、`/config` 命令。
- 移植完成后建议运行：`cargo build` + `cargo test -p zeroclaw-runtime -p zeroclaw-channels`。
