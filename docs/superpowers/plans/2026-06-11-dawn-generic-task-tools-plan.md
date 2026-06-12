# Dawn 通用任务工具实施计划

> **关联 Spec**: `docs/superpowers/specs/2026-06-11-dawn-generic-task-tools-design.md`  
> **分支**: `cjj`  
> **状态**: 已完成

## Task 1: Dawn Agents 配置结构

**文件**:
- 创建: `crates/zeroclaw-config/src/dawn_agents.rs`
- 修改: `crates/zeroclaw-config/src/schema.rs`
- 修改: `crates/zeroclaw-config/src/lib.rs`

实施内容：
- 定义 `DawnAgentConfig` 结构体（uid, name, description）
- 定义 `DawnAgents` 结构体（`HashMap<String, DawnAgentConfig>` + `#[serde(flatten)]`）
- 实现 `get_by_type(u8)` 方法
- 在 `Config` 结构体中添加 `dawn_agents` 字段

## Task 2: 重命名工具（xuanji → dawn）

**文件**:
- 创建: `crates/zeroclaw-runtime/src/tools/dawn_task.rs`
- 删除: `crates/zeroclaw-runtime/src/tools/xuanji.rs`
- 修改: `crates/zeroclaw-runtime/src/tools/mod.rs`
- 修改: `crates/zeroclaw-runtime/src/daemon/mod.rs`
- 修改: `crates/zeroclaw-channels/src/orchestrator/mod.rs`

实施内容：
- `XuanjiCreateTask` → `DawnCreateTask`
- `XuanjiQueryTask` → `DawnQueryTask`
- `XuanjiContext` → `DawnContext`
- `XUANJI_BRIDGE` → `DAWN_BRIDGE`
- `set_xuanji_bridge` → `set_dawn_bridge`
- 硬编码 uid → 通过 `DawnAgents.get_by_type()` 查找
- WK CMD 命令名: `xuanji.create_task` → `dawn.create_task`

## Task 3: Tool 描述优化

**文件**: `crates/zeroclaw-runtime/src/tools/dawn_task.rs`

实施内容：
- 增强 `description()` 包含 type=1 的具体参数格式示例
- 提示"请严格参考对应 Agent 的技能文档"

## Task 4: Skill 文档更新

**文件**: `dawn-xuanji-doc/SKILL.md`

实施内容：
- 技能名从 `extracting-xuanji-documents`（保持但更新内容）
- 调用工具名从 `dawn_xuanji_create_task` → `dawn_create_task`
- 增加参数格式规范章节（正确 vs 错误格式对比）
- 增加"调用前必须仔细分析"提示
- 强调 `files` 必须是数组

## Task 5: xuanji-agent 端适配

**文件**: `xuanji-agent/backend/src/app.py`

实施内容：
- CMD handler 从 `xuanji.create_task` → `dawn.create_task`
- handler 名称从 `handle_xuanji_create_task` → `handle_create_task`
- 兼容新旧参数格式（`params.files` vs 顶层 `files`）

## Task 6: 日志清理

**文件**:
- `crates/zeroclaw-channels/src/orchestrator/mod.rs`
- `crates/zeroclaw-runtime/src/daemon/mod.rs`

实施内容：
- "Xuanji" → "Tool"（bridge 相关日志和注释）

## Task 7: 编译验证

```bash
cd yumc_zeroclaw
cargo build --release
```

## 验证方法

1. 启动 xuanji-agent，确认 `dawn.create_task` CMD handler 正常注册
2. 通过 yumclaw 发送文档提取请求
3. 检查 yumclaw 日志：`dawn_create_task` 参数格式
4. 检查 xuanji-agent 日志：`handle_create_task` 处理成功
