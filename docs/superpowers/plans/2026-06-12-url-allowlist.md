# URL 白名单 — 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 在 config.toml 中添加 URL 白名单，同时作用于 LeakDetector 和 scrub_credentials，消除 URL 参数误报

**Architecture:** 统一使用 `LeakDetectorConfig.url_allowlist`，LeakDetector 和 scrub_credentials 共用白名单规则

**Spec:** `docs/superpowers/specs/2026-06-12-url-allowlist-design.md`

**Tech Stack:** Rust 2024, serde, regex, zeroclaw-config derivable

---

## 文件结构

| 文件 | 职责 | 状态 |
|------|------|------|
| `crates/zeroclaw-config/src/schema.rs` | `LeakDetectorConfig`、`UrlAllowlistEntry` 结构体 | ✅ 已完成 |
| `crates/zeroclaw-runtime/src/security/leak_detector.rs` | `from_config()`、`strip_whitelist_urls()`、辅助函数 | ✅ 已完成 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | `LeakDetector::from_config()` | ✅ 已完成 |
| `crates/zeroclaw-runtime/src/agent/loop_.rs` | `scrub_credentials()` 添加白名单支持 | 🔲 TODO |

---

## 已完成 (Task 1-5)

LeakDetector 修改已完成：schema 结构体、from_config、strip_whitelist_urls、scan 修改、orchestrator 调用点。

---

## Task 6: 修改 scrub_credentials 与 leak_detector（强保证保留）

**Files:**
- Modify: `crates/zeroclaw-runtime/src/security/leak_detector.rs`
- Modify: `crates/zeroclaw-runtime/src/agent/loop_.rs`
- Modify: `crates/zeroclaw-runtime/src/agent/tool_execution.rs`
- Modify: `crates/zeroclaw-runtime/src/security/mod.rs`
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

### 方案

采用统一“掩码 → 检测/清洗 → 还原”策略，确保白名单 URL 强保证保留。

### Step 1: leak_detector 改为掩码-还原

- 新增 `mask_allowlist_urls` / `restore_allowlist_urls`
- `scan()` 改为在 masked 文本上检测与替换，再 restore
- 删除 `strip_urls_by_allowlist`（日志专用预 strip 方案废弃）

### Step 2: loop/tool_execution 使用 allowlist task-local

- `run_tool_call_loop` 内部从 task-local 读取 `LeakDetectorConfig`
- 构建 `allowlist_rules` 并注入 `TOOL_LOOP_ALLOWLIST`
- `tool_execution::execute_one_tool` 继续使用 `scrub_credentials_with_allowlist`
- 日志 `tool_call_result.output` 恢复为 `scrub_credentials(&outcome.output)`

### Step 3: 缩短 leak_detector_config 参数链

- 新增 `TOOL_LOOP_LEAK_DETECTOR_CONFIG` task-local
- 在 orchestrator 入口 `.scope(Arc<LeakDetectorConfig>, run_tool_call_loop(...))`
- 删除 `run_tool_call_loop` 末尾的 `leak_detector_config` 参数及所有 `None` 透传

### Step 4: 回归测试

新增 `whitelist_url_token_preserved_when_same_token_detected_elsewhere`：
- 同一 token 同时出现在普通文本和白名单 URL
- 断言普通文本被 redacted，白名单 URL 完整保留

### Step 5: 编译与测试

```bash
cargo test -p zeroclaw-runtime security::leak_detector::tests::
cargo test -p zeroclaw-channels sanitize_channel_response
cargo build --release --bin zeroclaw
```

---

## 已完成任务摘要

| Task | 内容 | 状态 |
|------|------|------|
| 1 | schema.rs 新增 LeakDetectorConfig、UrlAllowlistEntry | ✅ |
| 2 | leak_detector.rs 修改 | ✅ |
| 3 | orchestrator 调用点 | ✅ |
| 4 | 测试添加 | ✅ |
| 5 | 编译测试 | ✅ |
| 6 | scrub_credentials 白名单（掩码-还原 + task-local 参数链优化） | ✅ |
