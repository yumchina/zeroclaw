# 任务本地变量（Task-Local）安全访问实施计划

> **对于 Agent 执行者：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans` 技能来逐步执行本计划。步骤使用复选框 (`- [ ]`) 语法进行跟踪。

**目标：** 避免在 Agent 启动或访问未初始化任务本地变量时触发 `cannot access a task-local storage value without setting it first` 的恐慌（panic）。

**架构：** 将 `CURRENT_TOOL_CALL_ID` 和 `CURRENT_TOOL_NAME` 的直接访问方法 `.with(|v| v.clone())` 替换为安全的 `.try_with(Clone::clone).ok().flatten()`。这可以在变量未设置时安全地返回 `None` 而不触发 panic。

**技术栈：** Rust (Tokio)

---

### 任务 1：修复 ProgressReportingObserver 任务本地变量访问

**文件：**
- 修改：`crates/zeroclaw-progress-observer/src/observer.rs:88-89`
- 测试：`crates/zeroclaw-progress-observer/src/observer.rs`（特别是 `emits_status_for_agent_start_and_passes_to_inner` 测试用例）

- [ ] **步骤 1：写入最简改动**

修改 `crates/zeroclaw-progress-observer/src/observer.rs` 第 88-89 行为：
```rust
            let tool_call_id = zeroclaw_api::CURRENT_TOOL_CALL_ID.try_with(Clone::clone).ok().flatten();
            let tool_name = zeroclaw_api::CURRENT_TOOL_NAME.try_with(Clone::clone).ok().flatten();
```

- [ ] **步骤 2：运行测试验证是否通过**

运行：`cargo test -p zeroclaw-progress-observer`
期望输出：
```
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
```

- [ ] **步骤 3：提交修改**

```bash
git add crates/zeroclaw-progress-observer/src/observer.rs
git commit -m "fix(observer): use safe try_with for task-local CURRENT_TOOL_CALL_ID and CURRENT_TOOL_NAME"
```

---

### 任务 2：修复 WuKongIMChannel 任务本地变量访问

**文件：**
- 修改：`crates/zeroclaw-channel-wukongim/src/channel.rs:1226-1227`

- [ ] **步骤 1：写入最简改动**

修改 `crates/zeroclaw-channel-wukongim/src/channel.rs` 第 1226-1227 行为：
```rust
        // Read from task locals
        let tool_call_id = zeroclaw_api::CURRENT_TOOL_CALL_ID.try_with(Clone::clone).ok().flatten();
        let tool_name = zeroclaw_api::CURRENT_TOOL_NAME.try_with(Clone::clone).ok().flatten();
```

- [ ] **步骤 2：验证构建和运行相关测试**

运行：`cargo test -p zeroclaw-channel-wukongim` 以及 `cargo clippy --package zeroclaw-channel-wukongim --all-targets -- -D warnings`
期望结果：所有编译通过且测试成功，无错误与警告。

- [ ] **步骤 3：提交修改**

```bash
git add crates/zeroclaw-channel-wukongim/src/channel.rs
git commit -m "fix(wukongim): use safe try_with for task-local access in send_status_update"
```
