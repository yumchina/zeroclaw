# master → 0.8.0 合并 · scrubbing 层迁移 — 实施 Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `upstream/master`（含 `89e24cc3a` 渲染层 scrubbing 重构）合入本地 `0.8.0`，期间解决 8 个文件 ~15 处冲突；并把本地 PR #48 的 URL allowlist 接入点从工具执行层迁移到渲染层，使 PR #48 想解决的"支付链接被打码"场景在新架构下继续工作。

**Architecture:** 两阶段。Phase A 解决合并冲突 —— Skill 结构体字段冲突（7 个文件）保留双方字段；`tool_execution.rs` 4 处架构冲突全部采用 upstream 版本（删除 `let scrubbed_reason = …`）。Phase B 在 3 个渲染边界（`turn/events.rs::emit_tool_result`、`turn/post_exec.rs::record_executed_outcomes`、`turn/post_exec.rs::render_completion_progress`）把 `scrub_credentials(...)` 替换为 `scrub_credentials_with_allowlist(..., &current_allowlist())`，并把 PR #48 在 `tool_execution.rs` 模块的两个集成测试搬到 `turn/redact.rs` 模块。

**Tech Stack:** Rust 2024、`tokio::task_local!`、`regex`。无新依赖。

**Reference materials:**
- 设计 spec：[../specs/2026-06-21-master-to-080-merge-redact-rewire-design.md](../specs/2026-06-21-master-to-080-merge-redact-rewire-design.md)
- 上游 PR：commit `89e24cc3a`（`git show 89e24cc3a`）
- 本地 PR #48 plan：[2026-06-17-url-allowlist-migration.md](2026-06-17-url-allowlist-migration.md)
- 本地 PR #48 关键 commit：`e75b2af15`（`refactor(security): route scrub_credentials call sites through TOOL_LOOP_ALLOWLIST`）

## Global Constraints

- **远程配置**：upstream remote = `https://github.com/zeroclaw-labs/zeroclaw.git`；当前分支 = `0.8.0`；目标合并 ref = `upstream/master`。
- **现有 PR #48 基础设施完整保留**：`crates/zeroclaw-runtime/src/agent/scrub_context.rs`（含 `TOOL_LOOP_ALLOWLIST` + `current_allowlist()`）、`security::AllowlistRule`、`security::{mask_allowlist_urls, restore_allowlist_urls, allowlist_from_config, LeakDetector::from_config}`、`turn/redact.rs::scrub_credentials_with_allowlist(input, rules)` —— 一行不动。
- **`tool_execution.rs` 接受 upstream 形态**：4 处 `let scrubbed_reason = scrub_for_tool_output(&reason)` / `let output = scrub_for_tool_output(normalized_output)` **全部删除**；保留 upstream 的 inline `scrub_credentials(&reason)` / 原始 `reason`。`scrub_for_tool_output` 函数本身（`tool_execution.rs:24`）保留 —— `call_prep.rs` 的两处仍在用。
- **3 个渲染调用点**强制走 `scrub_credentials_with_allowlist(x, &current_allowlist())`，明示读 task-local，不修改 `scrub_credentials` 函数本身。
- **commit 分界**：merge 冲突解决 = 1 个 merge commit；rendering 层接入 + 测试搬家 = 1 个 follow-up commit。绝不混。
- **TDD**：Phase B 每个调用点改动前先写失败测试（验证打码行为 / 验证 allowlist 旁路）。
- **comment policy**：不写 WHAT、不写 caller 列表；只在 non-obvious WHY 处加注释。
- **conventional commits**：merge commit 走默认 `Merge ...` 形态；follow-up commit 用 `refactor(security): rewire URL allowlist into the rendering layer (post-merge)`。
- **绝不 `--no-verify` / `--no-edit`**；任何 hook 失败先 root-cause。

---

## File Structure

| File | 责任 | 阶段 / 动作 |
|---|---|---|
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | Skill 字面量 3 处冲突 → 保留双方字段 | Phase A · Modify |
| `crates/zeroclaw-runtime/src/agent/agent.rs` | Skill 字面量 2 处冲突 → 保留双方字段 | Phase A · Modify |
| `crates/zeroclaw-runtime/src/agent/prompt.rs` | Skill 字面量 3 处冲突 → 保留双方字段 | Phase A · Modify |
| `crates/zeroclaw-runtime/src/skills/cache.rs` | Skill 字面量 1 处冲突 → 保留双方字段 | Phase A · Modify |
| `crates/zeroclaw-runtime/src/skills/mod.rs` | Skill 结构体定义 + manifest + builder 6 处冲突 → 保留双方字段 | Phase A · Modify |
| `crates/zeroclaw-runtime/src/skills/suggestions.rs` | Skill 字面量 1 处冲突 → 保留双方字段 | Phase A · Modify |
| `crates/zeroclaw-runtime/src/tools/skill_tool.rs` | Skill 字面量 1 处冲突 → 保留双方字段 | Phase A · Modify |
| `crates/zeroclaw-runtime/src/agent/tool_execution.rs` | 4 处架构冲突 → 全部采用 upstream；删除 PR #48 的两个集成测试 `mod allowlist_integration_tests`（搬到 `turn/redact.rs`） | Phase A · Modify ／ Phase B · Modify |
| `crates/zeroclaw-runtime/src/agent/turn/events.rs` | `emit_tool_result` 内 `scrub_credentials(&outcome.output)` → `scrub_credentials_with_allowlist(&outcome.output, &current_allowlist())` | Phase B · Modify |
| `crates/zeroclaw-runtime/src/agent/turn/post_exec.rs` | `record_executed_outcomes` 内 `output` / `error_reason` 字段 + `render_completion_progress` 内 `error_reason` 全部走 `_with_allowlist + current_allowlist()` | Phase B · Modify |
| `crates/zeroclaw-runtime/src/agent/turn/redact.rs` | 接收从 `tool_execution.rs` 搬来的 2 个 PR #48 集成测试 | Phase B · Modify |

---

## Phase A — 解决合并冲突

> 当前仓库状态：`git merge upstream/master` 已经跑过、产生冲突、未提交。working tree 含 8 个 `UU` 文件。**Phase A 的所有改动都在已存在的冲突标记区段内**。

### Task A0：前置确认（环境就绪 + 备份）

**Files:** 无（只读 + 备份）

**Interfaces:** 无

- [ ] **Step 1：确认当前处于已冲突的 merge 状态**

```bash
git status --short | head -20
```

期望输出包含 `UU crates/zeroclaw-channels/src/orchestrator/mod.rs` 等 8 行（顺序不限）。如果看不到 `UU` 行，说明上一次合并未保留 —— 先重新跑：

```bash
git fetch upstream master && git merge upstream/master
```

预期再次失败并列出同样 8 个冲突文件。

- [ ] **Step 2：备份未跟踪的本地输出**

```bash
git stash push --include-untracked -m "pre-merge-conflict-resolution backup"
git stash pop
```

`stash push --include-untracked` 后立刻 `pop`：作用是确认 untracked 文件（`test_output_full.txt` / `test_output_new.txt`）不会在后续操作中被误删。这两个文件是历史调试输出，与本次 merge 无关。

- [ ] **Step 3：导出冲突摘要供 review 备查**

```bash
git diff --name-only --diff-filter=U > /tmp/merge_conflicts.txt
cat /tmp/merge_conflicts.txt
```

期望 8 行输出（与上面 File Structure 表 Phase A 行一致）。

### Task A1：`crates/zeroclaw-runtime/src/skills/mod.rs` — Skill 结构体定义

> **从结构体定义开始**：这是其余 7 个文件的所有字面量冲突的"上游" —— 如果先改字面量、结构体定义后改，会出现类型不一致的中间状态。

**Files:**
- Modify: `crates/zeroclaw-runtime/src/skills/mod.rs`（6 处冲突，行号见下）

**Interfaces:**
- Consumes: 无
- Produces: `Skill` 与 `SkillManifest`（具体名以现场为准）结构体新增**两个**字段：`enabled: bool` 和 `slash_options: Vec<SkillSlashOption>`，**共存**

- [ ] **Step 1：定位 6 处冲突**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/skills/mod.rs
```

期望输出 6 组（每组 3 行），起始行 ≈ 70 / 197 / 1085 / 1116 / 1160 / 3163（合并后行号可能微调，以实际为准）。

- [ ] **Step 2：第 1 处 — Skill 结构体定义（≈ line 70）**

打开文件，定位形如：

```rust
    #[serde(default)]
    pub prompts: Vec<String>,
<<<<<<< HEAD
    /// When `false`, the skill is loaded so it shows in `zeroclaw skills list`
    /// (with a `[disabled]` badge), but its prompt content is NOT injected
    /// into the system prompt and its tool definitions are NOT registered.
    /// Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
=======
    /// Typed slash-command options a `slash`-tagged skill exposes (e.g. on
    /// Discord). Empty for skills that take no structured input — slash channels
    /// then fall back to a single free-text option. See [`SkillSlashOption`].
    #[serde(default)]
    pub slash_options: Vec<SkillSlashOption>,
>>>>>>> upstream/master
    #[serde(skip)]
    pub location: Option<PathBuf>,
```

替换为（**双方字段都保留，HEAD 字段在前**，顺序不重要但保持稳定有助于后续 review）：

```rust
    #[serde(default)]
    pub prompts: Vec<String>,
    /// When `false`, the skill is loaded so it shows in `zeroclaw skills list`
    /// (with a `[disabled]` badge), but its prompt content is NOT injected
    /// into the system prompt and its tool definitions are NOT registered.
    /// Default: `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Typed slash-command options a `slash`-tagged skill exposes (e.g. on
    /// Discord). Empty for skills that take no structured input — slash channels
    /// then fall back to a single free-text option. See [`SkillSlashOption`].
    #[serde(default)]
    pub slash_options: Vec<SkillSlashOption>,
    #[serde(skip)]
    pub location: Option<PathBuf>,
```

- [ ] **Step 3：第 2 处 — SkillManifest 字段（≈ line 197）**

定位形如（结构体名可能是 `SkillManifestSection` / `SkillMeta` 等 —— 以文件内实际名为准；本步关心的是字段块）：

```rust
    #[serde(default)]
    prompts: Vec<String>,
<<<<<<< HEAD
    #[serde(default = "default_true")]
    enabled: bool,
=======
    #[serde(default)]
    slash_options: Vec<SkillSlashOption>,
>>>>>>> upstream/master
}
```

替换为：

```rust
    #[serde(default)]
    prompts: Vec<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    slash_options: Vec<SkillSlashOption>,
}
```

- [ ] **Step 4：第 3 处 — SKILL.toml builder（≈ line 1085）**

定位形如：

```rust
        tools: manifest.tools,
        prompts,
<<<<<<< HEAD
        enabled: manifest.skill.enabled,
=======
        slash_options: manifest.skill.slash_options,
>>>>>>> upstream/master
        location: Some(path.to_path_buf()),
```

替换为：

```rust
        tools: manifest.tools,
        prompts,
        enabled: manifest.skill.enabled,
        slash_options: manifest.skill.slash_options,
        location: Some(path.to_path_buf()),
```

- [ ] **Step 5：第 4 处 — `load_skill_md` builder（≈ line 1116）**

定位形如：

```rust
        tools: Vec::new(),
        prompts: vec![parsed.body],
<<<<<<< HEAD
        enabled: parsed.meta.enabled.unwrap_or(true),
=======
        slash_options: Vec::new(),
>>>>>>> upstream/master
        location: Some(path.to_path_buf()),
```

替换为：

```rust
        tools: Vec::new(),
        prompts: vec![parsed.body],
        enabled: parsed.meta.enabled.unwrap_or(true),
        slash_options: Vec::new(),
        location: Some(path.to_path_buf()),
```

- [ ] **Step 6：第 5 处 — `load_open_skill_md` builder（≈ line 1160）**

定位形如：

```rust
        tools: Vec::new(),
        prompts: vec![parsed.body],
<<<<<<< HEAD
        enabled: true, // open-skills ignore per-file enabled; controlled at the repo level
=======
        slash_options: Vec::new(),
>>>>>>> upstream/master
        location: Some(path.to_path_buf()),
```

替换为：

```rust
        tools: Vec::new(),
        prompts: vec![parsed.body],
        enabled: true, // open-skills ignore per-file enabled; controlled at the repo level
        slash_options: Vec::new(),
        location: Some(path.to_path_buf()),
```

- [ ] **Step 7：第 6 处 — 内联测试字面量（≈ line 3163）**

定位形如：

```rust
            tools: vec![tool("run.lint", "shell")],
            prompts: Vec::new(),
<<<<<<< HEAD
            enabled: true,
=======
            slash_options: Vec::new(),
>>>>>>> upstream/master
            location: None,
```

替换为：

```rust
            tools: vec![tool("run.lint", "shell")],
            prompts: Vec::new(),
            enabled: true,
            slash_options: Vec::new(),
            location: None,
```

- [ ] **Step 8：验证 6 处都解完，无残留冲突标记**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/skills/mod.rs
```

期望：**空输出**。

- [ ] **Step 9：单独编译 skills 模块**

```bash
cargo check -p zeroclaw-runtime --lib 2>&1 | head -50
```

期望：无 `error[`。**警告 OK**（后续 task 文件还有冲突未解，所以编译会因为字面量缺字段而报错 —— 这是预期的，下一个 task 修；只要 `skills/mod.rs` 自身错误为 0 即可）。

> 注：本步不 commit —— Phase A 最后统一 commit。

### Task A2：`crates/zeroclaw-runtime/src/skills/cache.rs` — 1 处字面量

**Files:**
- Modify: `crates/zeroclaw-runtime/src/skills/cache.rs:253`

**Interfaces:**
- Consumes: A1 已加的两个字段
- Produces: 无

- [ ] **Step 1：定位**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/skills/cache.rs
```

期望 3 行：`<<<<<<< HEAD` / `=======` / `>>>>>>> upstream/master`。

- [ ] **Step 2：替换**

把：

```rust
                tools: vec![],
                prompts: vec![],
<<<<<<< HEAD
                enabled: true,
=======
                slash_options: vec![],
>>>>>>> upstream/master
                location: None,
```

改为：

```rust
                tools: vec![],
                prompts: vec![],
                enabled: true,
                slash_options: vec![],
                location: None,
```

- [ ] **Step 3：验证**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/skills/cache.rs
```

期望：空输出。

### Task A3：`crates/zeroclaw-runtime/src/skills/suggestions.rs` — 1 处字面量

**Files:**
- Modify: `crates/zeroclaw-runtime/src/skills/suggestions.rs:312`

**Interfaces:** 同 A2

- [ ] **Step 1：定位 + Step 2：替换**

定位形如：

```rust
            tools: vec![],
            prompts: vec![],
<<<<<<< HEAD
            enabled: true,
=======
            slash_options: Vec::new(),
>>>>>>> upstream/master
            location: None,
```

替换为：

```rust
            tools: vec![],
            prompts: vec![],
            enabled: true,
            slash_options: Vec::new(),
            location: None,
```

- [ ] **Step 3：验证**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/skills/suggestions.rs
```

期望：空输出。

### Task A4：`crates/zeroclaw-runtime/src/tools/skill_tool.rs` — 1 处字面量

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/skill_tool.rs:1121`

**Interfaces:** 同 A2

- [ ] **Step 1：定位 + Step 2：替换**

```rust
            }],
            prompts: vec![],
<<<<<<< HEAD
            enabled: true,
=======
            slash_options: Vec::new(),
>>>>>>> upstream/master
            location: None,
```

→

```rust
            }],
            prompts: vec![],
            enabled: true,
            slash_options: Vec::new(),
            location: None,
```

- [ ] **Step 3：验证空冲突标记**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/tools/skill_tool.rs
```

### Task A5：`crates/zeroclaw-runtime/src/agent/prompt.rs` — 3 处字面量

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/prompt.rs:488, 540, 625`

**Interfaces:** 同 A2

- [ ] **Step 1：定位 3 处**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/agent/prompt.rs
```

期望 9 行（3 组 × 3）。

- [ ] **Step 2：3 处统一替换 pattern**

每处都是同样 pattern：

```rust
            prompts: vec!["...".into()],
<<<<<<< HEAD
            enabled: true,
=======
            slash_options: Vec::new(),
>>>>>>> upstream/master
            location: ...,
```

→

```rust
            prompts: vec!["...".into()],
            enabled: true,
            slash_options: Vec::new(),
            location: ...,
```

逐一处理 3 处。

- [ ] **Step 3：验证空冲突标记**

### Task A6：`crates/zeroclaw-runtime/src/agent/agent.rs` — 2 处字面量

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/agent.rs:6121, 6214`

**Interfaces:** 同 A2

- [ ] **Step 1 + 2 + 3：定位 2 处、逐一替换 pattern（同 A5）、验证空冲突标记**

### Task A7：`crates/zeroclaw-channels/src/orchestrator/mod.rs` — 3 处字面量

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs:16376, 16422, 16478`

**Interfaces:** 同 A2

- [ ] **Step 1 + 2 + 3：定位 3 处、逐一替换 pattern（同 A5）、验证空冲突标记**

### Task A8：`crates/zeroclaw-runtime/src/agent/tool_execution.rs` — 4 处架构冲突（全部采用 upstream）

> 本 task 是 Phase A 唯一的**真正裁决**点。决定原因见 spec §5 决定 #1。

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/tool_execution.rs:112, 243, 270, 311`

**Interfaces:**
- Consumes: 无
- Produces:
  - `tool_execution.rs` 4 个错误分支不再持有 `let scrubbed_reason = …` / `let output = …` 中间变量
  - `scrub_for_tool_output` 函数本身（line 24 附近）**保留不动**（`call_prep.rs:10` 仍在用）
  - 后续 `observer.record_event(...)` 调用走 upstream 版 `Some(scrub_credentials(&reason))`、`Some(scrub_credentials(normalized_output))`、`Some(scrub_credentials(&reason))`、`Some(scrub_credentials(&reason))`，`error_reason: Some(reason)` / `output: reason.clone()`（已由 auto-merge 落到 working tree）

- [ ] **Step 1：确认 4 处冲突的形态**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/agent/tool_execution.rs
```

期望 12 行（4 组 × 3）。

- [ ] **Step 2：第 1 处（≈ line 112，"Unknown tool" 分支）**

定位形如：

```rust
        let reason = format!("Unknown tool: {call_name}");
        let duration = start.elapsed();
<<<<<<< HEAD
        let scrubbed_reason = scrub_for_tool_output(&reason);
=======
>>>>>>> upstream/master
        observer.record_event(&ObserverEvent::ToolCall {
```

替换为（删除整组冲突标记 + HEAD 行）：

```rust
        let reason = format!("Unknown tool: {call_name}");
        let duration = start.elapsed();
        observer.record_event(&ObserverEvent::ToolCall {
```

- [ ] **Step 3：第 2 处（≈ line 243，成功分支）**

```rust
                    let normalized_output = if r.output.is_empty() {
                        "(no output)"
                    } else {
                        &r.output
                    };
<<<<<<< HEAD
                    let output = scrub_for_tool_output(normalized_output);
=======
>>>>>>> upstream/master
                    let receipt = receipt_generator.map(|receipt_gen| {
```

→

```rust
                    let normalized_output = if r.output.is_empty() {
                        "(no output)"
                    } else {
                        &r.output
                    };
                    let receipt = receipt_generator.map(|receipt_gen| {
```

- [ ] **Step 4：第 3 处（≈ line 270，工具内部失败分支）**

```rust
                } else {
                    let reason = r.error.unwrap_or(r.output);
<<<<<<< HEAD
                    let scrubbed_reason = scrub_for_tool_output(&reason);
=======
>>>>>>> upstream/master
                    observer.record_event(&ObserverEvent::ToolCall {
```

→

```rust
                } else {
                    let reason = r.error.unwrap_or(r.output);
                    observer.record_event(&ObserverEvent::ToolCall {
```

- [ ] **Step 5：第 4 处（≈ line 311，工具 `Err(e)` 分支）**

```rust
                let reason = format!("Error executing {call_name}: {e}");
<<<<<<< HEAD
                let scrubbed_reason = scrub_for_tool_output(&reason);
=======
>>>>>>> upstream/master
                observer.record_event(&ObserverEvent::ToolCall {
```

→

```rust
                let reason = format!("Error executing {call_name}: {e}");
                observer.record_event(&ObserverEvent::ToolCall {
```

- [ ] **Step 6：验证空冲突标记**

```bash
grep -nE '^(<<<<<<<|=======|>>>>>>>)' crates/zeroclaw-runtime/src/agent/tool_execution.rs
```

期望：空输出。

- [ ] **Step 7：确认 `scrub_for_tool_output` 仍被引用**

```bash
grep -n 'scrub_for_tool_output' crates/zeroclaw-runtime/src/
```

期望仍能看到 `agent/tool_execution.rs:24`（函数定义）+ `agent/turn/call_prep.rs:{10,74}`（`use` + 调用）+ `agent/tool_execution.rs` 的 2 个测试调用（≈ line 491、506）。**不应**再看到 line 112 / 243 / 270 / 311 处的非测试调用。

如果发现 `scrub_for_tool_output` 已无任何非测试 caller，**不要**顺手删函数 —— 测试还在用，且 follow-up Phase B 不依赖删除它。函数留着，零成本。

### Task A9：全仓库无残留冲突 + 单元/集成测试 + commit

**Files:** 无（仅命令）

**Interfaces:** 无

- [ ] **Step 1：全仓库扫一遍冲突标记**

```bash
grep -rnE '^(<<<<<<<|=======|>>>>>>>)' --include='*.rs' crates/ src/ 2>/dev/null | head
```

期望：**空输出**（注意 `grep` 自身 `^=======` 模式不会匹配到 Markdown 分隔线 —— 仅过滤 `.rs`）。

- [ ] **Step 2：workspace check**

```bash
cargo check --workspace 2>&1 | tail -20
```

期望：`Finished` / 无 `error[`。**警告允许**（但记录数量，Step 3 之后再回头看是否新增）。

- [ ] **Step 3：跑 runtime / channels 相关 test**

```bash
cargo test -p zeroclaw-runtime --lib 2>&1 | tail -10
cargo test -p zeroclaw-channels --lib 2>&1 | tail -10
```

期望：两条都 `test result: ok`，0 fail。

> ⚠️ 如果有 fail，**首先怀疑**自己冲突解错了 —— 跑 `git diff --check` 与 `git diff upstream/master HEAD -- <fail-file>` 比对预期。**不要**用 `--skip` / `#[ignore]` 绕过。

- [ ] **Step 4：完整 workspace test**

```bash
cargo test --workspace 2>&1 | tail -20
```

期望：所有 crate `test result: ok`。

- [ ] **Step 5：合成 merge commit**

```bash
git add -A
git status --short  # 二次确认：所有 8 文件都从 UU → M / staged
```

期望 `git status --short` 显示 `M  ` 而非 `UU` / `AA` 等。

```bash
git commit -m "$(cat <<'EOF'
Merge upstream/master into 0.8.0

Resolves 8 conflict files (~15 hunks):

- skills/mod.rs + 6 dependents (orchestrator, agent, prompt, cache,
  suggestions, skill_tool): keep BOTH HEAD's `Skill.enabled` and
  upstream's `Skill.slash_options` — orthogonal, both required.

- tool_execution.rs (4 hunks): accept upstream's removal of
  `let scrubbed_reason = scrub_for_tool_output(&reason)` — the local
  PR #48 allowlist hooks are obsolete at this layer per upstream
  89e24cc3a (credential redaction moved to the rendering layer; data
  path now carries raw bytes, HMAC receipts sign raw bytes).

PR #48's intent (URL allowlist for trusted hosts) is preserved by a
follow-up commit rewiring the allowlist into the 3 rendering call
sites (turn/events.rs::emit_tool_result, turn/post_exec.rs::
record_executed_outcomes, turn/post_exec.rs::render_completion_progress).

See docs/superpowers/specs/2026-06-21-master-to-080-merge-redact-rewire-design.md
for the full migration rationale.
EOF
)"
```

期望：commit 成功，`git log --oneline -3` 顶部看到该 merge commit 与上游历史会合。

> 如果 pre-commit hook 报错：**先 root-cause**（通常是格式化 / lint 报警），修复后**新 commit**（不 `--amend`）。不 `--no-verify`。

- [ ] **Step 6：标记 Phase A 完成**

`git log --oneline -1` 输出第一行包含 `Merge upstream/master into 0.8.0`，且 `git status` clean（只剩 untracked test 文件）即可进入 Phase B。

---

## Phase B — Rendering 层接入 URL allowlist

> Phase A 完成后，PR #48 的 allowlist 机制仍然完整（`scrub_credentials_with_allowlist` 函数 + `current_allowlist()` + `TOOL_LOOP_ALLOWLIST` scope），但 3 个渲染调用点还在用简形 `scrub_credentials(...)`，没读 task-local —— 这就是用户在 UI 上仍然看到打码 URL 的原因。Phase B 把它们补上。

### Task B0：基线确认

**Files:** 无

**Interfaces:** 无

- [ ] **Step 1：确认 3 个调用点的当前形态**

```bash
grep -n 'scrub_credentials' crates/zeroclaw-runtime/src/agent/turn/events.rs crates/zeroclaw-runtime/src/agent/turn/post_exec.rs
```

期望命中：

- `agent/turn/events.rs`：`use super::redact::scrub_credentials;` + `emit_tool_result` 内 1 处 `scrub_credentials(&outcome.output)`
- `agent/turn/post_exec.rs`：`use ... scrub_credentials;` + `record_executed_outcomes` 内 2 处（`output` + `error_reason`） + `render_completion_progress` 内 1 处（`error_reason`）

> 若实际数量与预期不符（upstream `89e24cc3a` 落地后还可能有微调），以代码现状为准 —— 每个 `scrub_credentials(...)` 调用都按 Task B1 / B2 模板处理。

- [ ] **Step 2：确认 `scrub_credentials_with_allowlist` 与 `current_allowlist` 可访问**

```bash
grep -n 'pub fn scrub_credentials_with_allowlist' crates/zeroclaw-runtime/src/agent/turn/redact.rs
grep -n 'pub fn current_allowlist' crates/zeroclaw-runtime/src/agent/scrub_context.rs
```

两条都应命中。如果没命中，说明 PR #48 基础设施在 merge 中被误删 —— 停下来回头审 Task A1 / A8。

### Task B1：`turn/events.rs::emit_tool_result` 接入 allowlist

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/turn/events.rs`
- Test: 同文件（追加到现有 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: `super::redact::scrub_credentials_with_allowlist`、`crate::agent::scrub_context::{TOOL_LOOP_ALLOWLIST, current_allowlist}`、`crate::security::AllowlistRule`
- Produces: `emit_tool_result` 内 `output` 字段走 allowlist 旁路

- [ ] **Step 1：写失败测试 —— allowlist 命中时 URL 完整保留**

定位 `events.rs` 现有的 `#[cfg(test)] mod tests` 块（`89e24cc3a` 已加 `tool_result_event_is_scrubbed_for_rendering` 示例 —— 仿照它）。在该 mod 末尾追加：

```rust
    use crate::agent::scrub_context::TOOL_LOOP_ALLOWLIST;
    use crate::security::AllowlistRule;
    use std::sync::Arc;

    /// allowlisted-host URL token in a tool result must survive the rendering
    /// scrub when the orchestrator has set TOOL_LOOP_ALLOWLIST.
    #[tokio::test]
    async fn tool_result_event_preserves_allowlisted_url_token() {
        let outcome = ToolExecutionOutcome {
            output: "Pay: https://api.example.com/o?token=hgnD0jgCF63abcdefghij ok"
                .into(),
            success: true,
            error_reason: None,
            duration: Duration::ZERO,
            receipt: None,
        };
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async {
                emit_tool_call_pair(&tx, &parsed_call(Some("c1")), &outcome).await;
            })
            .await;
        drop(tx);
        let mut saw = false;
        while let Some(ev) = rx.recv().await {
            if let TurnEvent::ToolResult { output, .. } = ev {
                saw = true;
                assert!(
                    output.contains("token=hgnD0jgCF63abcdefghij"),
                    "allowlisted token must survive: {output}"
                );
            }
        }
        assert!(saw, "a ToolResult event must be emitted");
    }
```

> 如果 `parsed_call` / `emit_tool_call_pair` 在测试 mod 内的辅助名不同，照 `tool_result_event_is_scrubbed_for_rendering` 复制即可。

- [ ] **Step 2：跑测试验证失败**

```bash
cargo test -p zeroclaw-runtime --lib agent::turn::events::tests::tool_result_event_preserves_allowlisted_url_token -- --nocapture 2>&1 | tail -20
```

期望 FAIL —— 失败信息形如 `allowlisted token must survive: Pay: https://api.example.com/o?token=hgnD*[REDACTED] ok`（因为简形 `scrub_credentials` 还在用，token 被打码）。

- [ ] **Step 3：把 `emit_tool_result` 内的 `scrub_credentials` 换成 allowlist 版**

定位文件顶部 `use`：

```rust
use super::redact::scrub_credentials;
```

替换为：

```rust
use super::redact::scrub_credentials_with_allowlist;
use crate::agent::scrub_context::current_allowlist;
```

定位 `emit_tool_result` 内部（≈ commit `89e24cc3a` patch 的 `output: scrub_credentials(&outcome.output)` 处）：

```rust
            output: scrub_credentials(&outcome.output),
```

替换为：

```rust
            output: scrub_credentials_with_allowlist(&outcome.output, &current_allowlist()),
```

- [ ] **Step 4：跑测试验证通过 + 原回归测试仍 pass**

```bash
cargo test -p zeroclaw-runtime --lib agent::turn::events -- --nocapture 2>&1 | tail -20
```

期望：`tool_result_event_preserves_allowlisted_url_token` 与 `tool_result_event_is_scrubbed_for_rendering` 两条都 PASS。

### Task B2：`turn/post_exec.rs` 接入 allowlist（2 处 `record_executed_outcomes` + 1 处 `render_completion_progress`）

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/turn/post_exec.rs`
- Test: 同文件（追加到现有 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: 同 B1
- Produces: `record_executed_outcomes` 内 `output` 与 `error_reason` 字段、`render_completion_progress` 函数体 全部走 allowlist 旁路

- [ ] **Step 1：写失败测试 — `render_completion_progress` 在 allowlist scope 内保留 URL token**

追加到现有 tests mod：

```rust
    use crate::agent::scrub_context::TOOL_LOOP_ALLOWLIST;
    use crate::security::AllowlistRule;
    use std::sync::Arc;

    #[tokio::test]
    async fn completion_progress_preserves_allowlisted_url_in_error_reason() {
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let line = TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async {
                render_completion_progress(
                    "http_get",
                    1,
                    false,
                    Some("https://api.example.com/o?token=hgnD0jgCF63abcdefghij"),
                )
            })
            .await;
        assert!(
            line.contains("token=hgnD0jgCF63abcdefghij"),
            "allowlisted token must survive in progress line: {line}"
        );
    }
```

> 注：`render_completion_progress` 当前签名是同步函数。把它改为同步 `current_allowlist()` 调用即可 —— `current_allowlist()` 本身不是 async，仅 `TOOL_LOOP_ALLOWLIST.try_with(...)` 同步读，所以保持同步签名没问题。测试用 `TOOL_LOOP_ALLOWLIST::scope(async { sync_call })` 也合法（scope 是 async，但被 scope 的 future 内做同步 call 即可）。

- [ ] **Step 2：跑测试验证失败**

```bash
cargo test -p zeroclaw-runtime --lib agent::turn::post_exec::tests::completion_progress_preserves_allowlisted_url_in_error_reason -- --nocapture 2>&1 | tail -20
```

期望 FAIL。

- [ ] **Step 3：改 `use` + 3 处调用**

文件顶部 `use`：

```rust
use super::redact::scrub_credentials;
```

→

```rust
use super::redact::scrub_credentials_with_allowlist;
use crate::agent::scrub_context::current_allowlist;
```

`record_executed_outcomes` 内（2 处，按 `89e24cc3a` patch 是 `"error_reason": ...as_deref().map(scrub_credentials)` + `"output": scrub_credentials(&outcome.output)`）：

```rust
                    "error_reason": outcome.error_reason.as_deref().map(scrub_credentials),
                    "output": scrub_credentials(&outcome.output),
```

→

```rust
                    "error_reason": outcome
                        .error_reason
                        .as_deref()
                        .map(|s| scrub_credentials_with_allowlist(s, &current_allowlist())),
                    "output": scrub_credentials_with_allowlist(
                        &outcome.output,
                        &current_allowlist(),
                    ),
```

`render_completion_progress` 函数体内：

```rust
            truncate_with_ellipsis(&scrub_credentials(reason), 200)
```

→

```rust
            truncate_with_ellipsis(
                &scrub_credentials_with_allowlist(reason, &current_allowlist()),
                200,
            )
```

- [ ] **Step 4：跑测试验证通过 + 原 2 个回归测试仍 pass**

```bash
cargo test -p zeroclaw-runtime --lib agent::turn::post_exec -- --nocapture 2>&1 | tail -20
```

期望：`completion_progress_preserves_allowlisted_url_in_error_reason`、`completion_progress_scrubs_credential_error_reason`、`completion_progress_success_has_no_error_text` 三条 PASS。

### Task B3：把 PR #48 的 2 个集成测试从 `tool_execution.rs` 搬到 `turn/redact.rs`

> 这两个测试在 PR #48 commit `e75b2af15` 里加在 `tool_execution.rs` 末尾，测试目标是 `scrub_for_tool_output(raw)`。Phase A 已经删除 `tool_execution.rs` 里 `scrub_for_tool_output` 的非测试 caller —— 测试还在，但测试对象现在仅服务于 `call_prep.rs`，与 PR #48 的"渲染层 allowlist"语义无关。搬到 `turn/redact.rs` 改测 `scrub_credentials_with_allowlist` 是语义正确的归属。

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/tool_execution.rs`（删除 `#[cfg(test)] mod allowlist_integration_tests` 整块）
- Modify: `crates/zeroclaw-runtime/src/agent/turn/redact.rs`（在已有 `#[cfg(test)] mod tests` 末尾追加搬过来的 2 个测试，改测试目标）

**Interfaces:** 同 B1

- [ ] **Step 1：定位待搬走的 mod**

```bash
grep -n 'mod allowlist_integration_tests' crates/zeroclaw-runtime/src/agent/tool_execution.rs
```

期望：1 行命中（≈ line 480）。

- [ ] **Step 2：在 `turn/redact.rs::tests` 末尾追加搬家后的版本**

打开 `crates/zeroclaw-runtime/src/agent/turn/redact.rs`，在最后一个 `}`（关闭 `mod tests`）**之前**追加：

```rust
    use crate::agent::scrub_context::TOOL_LOOP_ALLOWLIST;
    use std::sync::Arc;

    /// End-to-end: when orchestrator sets TOOL_LOOP_ALLOWLIST and a renderer
    /// calls `scrub_credentials_with_allowlist(x, &current_allowlist())`,
    /// allowlisted URL tokens survive.
    #[tokio::test]
    async fn allowlisted_url_token_survives_rendering_scrub() {
        let raw = "QR: https://api.example.com/o?token=hgnD0jgCF63abcdefghij done";
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let scrubbed = TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async move {
                let rules = crate::agent::scrub_context::current_allowlist();
                scrub_credentials_with_allowlist(raw, &rules)
            })
            .await;
        assert!(
            scrubbed.contains("token=hgnD0jgCF63abcdefghij"),
            "allowlisted token must survive: {scrubbed}"
        );
    }

    #[tokio::test]
    async fn non_allowlisted_url_token_still_scrubbed_in_rendering() {
        let raw = "evil: https://evil.com/x?token=abcdefghijklmnop done";
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let scrubbed = TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async move {
                let rules = crate::agent::scrub_context::current_allowlist();
                scrub_credentials_with_allowlist(raw, &rules)
            })
            .await;
        assert!(!scrubbed.contains("abcdefghijklmnop"), "got: {scrubbed}");
    }
```

- [ ] **Step 3：跑搬家后测试**

```bash
cargo test -p zeroclaw-runtime --lib agent::turn::redact::tests::allowlisted_url_token_survives_rendering_scrub agent::turn::redact::tests::non_allowlisted_url_token_still_scrubbed_in_rendering -- --nocapture 2>&1 | tail -15
```

期望：2 条 PASS。

- [ ] **Step 4：从 `tool_execution.rs` 删除原 mod**

打开 `crates/zeroclaw-runtime/src/agent/tool_execution.rs`，定位 `#[cfg(test)] mod allowlist_integration_tests { ... }` 整块（约 line 480 至文件末尾或下一个顶层项），整块删除。

- [ ] **Step 5：验证删除后仍编译**

```bash
cargo check -p zeroclaw-runtime --tests 2>&1 | tail -10
```

期望：无 `error[`。

如果出现 `unused_imports` 警告（搬走后某些 `use` 不再被引用），按警告提示删掉对应 `use`。

### Task B4：workspace 验证 + commit

**Files:** 无

**Interfaces:** 无

- [ ] **Step 1：全 workspace test**

```bash
cargo test --workspace 2>&1 | tail -25
```

期望：所有 crate `test result: ok`，0 fail。

- [ ] **Step 2：（可选但推荐）clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

期望：0 warning（若仓库已有 baseline warning，与 Phase A 完成时对比无新增）。

- [ ] **Step 3：staged 文件确认**

```bash
git status --short
git diff --stat
```

期望：6 个文件改动 —— `agent/turn/events.rs` / `agent/turn/post_exec.rs` / `agent/turn/redact.rs` / `agent/tool_execution.rs`，且差异仅限于 Phase B 描述的范围（rendering 层 3 处替换 + 测试搬家 + 删除原测试 mod）。

- [ ] **Step 4：follow-up commit**

```bash
git add crates/zeroclaw-runtime/src/agent/turn/events.rs \
        crates/zeroclaw-runtime/src/agent/turn/post_exec.rs \
        crates/zeroclaw-runtime/src/agent/turn/redact.rs \
        crates/zeroclaw-runtime/src/agent/tool_execution.rs
git commit -m "$(cat <<'EOF'
refactor(security): rewire URL allowlist into the rendering layer (post-merge)

Following the merge of upstream 89e24cc3a (credential redaction moved
to the rendering layer), this commit re-points local PR #48's URL
allowlist mechanism at the 3 new rendering call sites:

- turn/events.rs::emit_tool_result
- turn/post_exec.rs::record_executed_outcomes (output + error_reason)
- turn/post_exec.rs::render_completion_progress (error_reason)

Each now calls scrub_credentials_with_allowlist(x, &current_allowlist())
instead of plain scrub_credentials(x). The TOOL_LOOP_ALLOWLIST scope set
by the orchestrator (orchestrator/mod.rs:4217) now reaches the renderers
that humans actually see, so allowlisted-host URL tokens (e.g. Luckin
payment links) survive on Discord / Lark / CLI / UI surfaces.

PR #48's two integration tests move from tool_execution.rs to
turn/redact.rs, retargeting scrub_credentials_with_allowlist directly.

Data path (LLM input, HMAC receipt) remains raw bytes as introduced
by upstream — this commit does not alter that.

See docs/superpowers/specs/2026-06-21-master-to-080-merge-redact-rewire-design.md
§6 for the new pipeline diagram.
EOF
)"
```

期望：commit 成功；`git log --oneline -2` 顶部为本 commit、之下为 Task A9 的 merge commit。

- [ ] **Step 5：（可选）手动场景回归**

如团队需要在合入主线前验证端到端体验：

1. 在 `~/.config/zeroclaw/config.toml`（或仓库 `examples/`）加：
   ```toml
   [[security.leak_detector.url_allowlist]]
   domain = "*.lkcoffee.com"
   ```
2. 跑 `cargo run --bin zeroclaw -- ...`（或对应入口），让 AI 触发返回 `payOrderQrCodeUrl` 的工具调用
3. 在 Discord / Lark / CLI 三个面分别检查 URL 是否完整呈现（含 token）
4. 同一会话内打 `api_key="sk-live-abcdefghijklmnop"`，确认仍被打码 `api_key="sk-l*[REDACTED]"`

通过即可推 PR。

---

## Self-Review

**1. Spec coverage：**

- spec §5 决定 #1（tool_execution 接受 upstream）→ Task A8
- spec §5 决定 #2（allowlist 接入渲染层）→ Task B1 + B2
- spec §5 决定 #3（保留 PR #48 基础设施）→ Global Constraints 第 2 条 + Task A8 Step 7 保留 `scrub_for_tool_output` 函数
- spec §5 决定 #4（loop_.rs:422 保留）→ 不在 8 个冲突文件中（auto-merge 已处理），Global Constraints 第 2 条覆盖
- spec §5 决定 #5（merge 与 follow-up 分两个 commit）→ Task A9 + Task B4
- spec §5 决定 #6（Skill 双字段共存）→ Task A1–A7
- spec §5 决定 #7（测试搬家）→ Task B3
- spec §9 验收：cargo build/test/手动场景 → Task A9 + Task B4

**2. Placeholder scan：** 全文检索 `TBD` / `TODO` / `implement later` / `fill in details` / `add appropriate error handling` / `similar to Task N` —— **无命中**。

**3. Type consistency：**

- `enabled: bool` / `slash_options: Vec<SkillSlashOption>` 在 Task A1 结构体定义 + Task A2–A7 所有字面量中一致
- `scrub_credentials_with_allowlist(input: &str, rules: &[AllowlistRule]) -> String` 与 `current_allowlist() -> Vec<AllowlistRule>` 签名在 Task B1 / B2 / B3 全部一致（已与现有 `turn/redact.rs` / `scrub_context.rs` 实现比对）
- `TOOL_LOOP_ALLOWLIST` 在 Task B1 / B2 / B3 测试中使用 `scope(Some(Arc::new(vec![rule])), async { ... })` 形态，与 `scrub_context.rs` 现有 doctest 一致

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-06-21-master-to-080-merge-redact-rewire.md`. Two execution options:

**1. Subagent-Driven (recommended)** — 主席（你）每个 task 分发一个新 subagent 实现，task 之间两阶段 review。Phase A 的 A1–A8 互相独立性强，特别适合并行；A9 / Phase B 顺序执行。

**2. Inline Execution** — 在当前会话里直接按 task 顺序执行，每 task 完成后 checkpoint。

选哪种？
