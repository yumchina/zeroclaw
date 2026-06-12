# Dawn 通用任务工具设计

> **状态**: 已实施 (cjj 分支)  
> **日期**: 2026-06-11

## 目标

将硬编码的璇玑专用工具（`dawn_xuanji_create_task`/`dawn_xuanji_query_task`）重构为通用的 Dawn Agent 任务工具（`dawn_create_task`/`dawn_query_task`），通过配置驱动的方式支持多种 Agent 类型。

## 动机

- 旧工具硬编码了 `xuanji_uid`，只能向璇玑 Agent 发任务
- 未来需要支持代码分析、数据处理等多种 Agent 类型
- 工具命名应为通用名称，不应包含具体 Agent 名称

## 架构

```
LLM → dawn_create_task(type=N) → DAWN_BRIDGE(mpsc) → orchestrator bridge listener → WuKongIM → agent_uid(N)
```

所有 type 走同一路径，通过 `DawnAgents.get_by_type(N)` 查找目标 agent_uid，经 WuKongIM 投递到对应 Agent。

### 关键机制：DAWN_CONTEXT

`tokio::task_local!` 存储 `from_uid` 和 `reply_target`，orchestrator 在每个 agent turn 通过 `DAWN_CONTEXT.scope()` 注入。Tool 执行时通过 `read_context()` 读取，用于构造消息中的 `reply_to`/`reply_target` 字段（含回退逻辑：空或不完整时回退为 `"1:{from_uid}"`）。

### 通信方式

- `channel_type` 固定为 `1`（WuKongIM 个人频道）
- 消息类型固定为 `type=2000`（CMD 消息）
- Bridge listener 仅在 `#[cfg(feature = "channel-wukongim")]` 时编译，由 mpsc `UnboundedSender`/`UnboundedReceiver` 解耦

### 组件

| 组件 | 文件 | 职责 |
|------|------|------|
| DawnAgents 配置 | `zeroclaw-config/src/dawn_agents.rs` | type→uid 映射（TOML key 为字符串，`get_by_type` 通过 `u8.to_string()` 查找） |
| DawnCreateTask | `zeroclaw-runtime/src/tools/dawn_task.rs` | 创建任务，通过 bridge 发送；同步返回纯文本确认，不含 task_id |
| DawnQueryTask | `zeroclaw-runtime/src/tools/dawn_task.rs` | 单向发送查询请求，任务状态通过异步 CMD 回调返回，非同步查询 |
| Bridge 初始化 | `zeroclaw-runtime/src/daemon/mod.rs` | daemon 启动时调用 `set_dawn_bridge()` 注入 mpsc sender |
| Bridge 消费 | `zeroclaw-channels/src/orchestrator/mod.rs` | bridge listener 接收消息并转发到 WuKongIM |
| DAWN_CONTEXT | `zeroclaw-runtime/src/tools/dawn_task.rs` | task-local 用户上下文（from_uid, reply_target） |
| xuanji-agent handler | `xuanji-agent/backend/src/app.py` | 接收 `dawn.create_task`/`dawn.query_task` CMD 并处理 |
| SKILL.md | `dawn-xuanji-doc/SKILL.md` | 每个 Agent 的参数格式文档 |

### 数据流

1. LLM 调用 `dawn_create_task(type=1, user_text, params)`
2. Tool 从 `read_context()` 获取用户上下文
3. Tool 通过 `DawnAgents.get_by_type(1)` 获取目标 agent_uid
4. 构造 WK CMD 消息 `{type:2000, cmd:"dawn.create_task", param:{type, user_id, user_text, params, reply_to, reply_target}}`
5. 通过 `DAWN_BRIDGE` mpsc channel 发送到 orchestrator
6. Orchestrator bridge listener 通过 WuKongIM 转发到目标 agent_uid
7. xuanji-agent 的 `handle_create_task` 接收并异步处理
8. **Tool 同步返回**纯文本确认（"已提交任务到 {name}"），**task_id 通过后续异步 CMD 回调通知**

## 配置

```toml
# config.toml
[dawn_agents.1]       # TOML key 为字符串 "1"，内部 HashMap<String, DawnAgentConfig>
uid = "1878_xuanji_agent"
name = "文档提取"
description = "提取并解析文档内容"

[dawn_agents.2]
uid = "xxx"
name = "代码分析"

[dawn_agents.3]
uid = "yyy"
name = "数据处理"
```

## 参数格式

```json
{
  "type": 1,
  "user_text": "用户原始文字",
  "params": {
    "files": [
      {"file_url": "...", "file_name": "...", "file_type": "pdf"}
    ]
  }
}
```

Tool schema 中 `params` 为泛型 `object`，`type` 的 enum `[1,2,3]` 和描述为硬编码（非从配置自动生成）。新增 type 需同步修改 schema 和 description。

## 已知限制

1. **params 为泛型 object**：nextg-std 模型可能传错格式（如 `{"files": {"item": {...}}}` 而非数组）
2. **type enum 硬编码**：`parameters_schema` 中 `"enum": [1, 2, 3]` 写死，新增 Agent type 需改代码
3. **同步返回不含 task_id**：`dawn_create_task` 只返回文本确认，task_id 通过后续异步通知传递。SKILL.md 中关于"从返回值提取 task_id"的描述与实际不符
4. **query_task 是单向发送**：不等待查询结果，结果通过 `xuanji.task_status` CMD 异步返回
5. **无 rate limiting**：Dawn 工具未包裹 `RateLimitedTool`/`PathGuardedTool`，LLM 可无限制发送

## 向后兼容

- Skill 文档已从 `dawn_xuanji_create_task` 更新为 `dawn_create_task`
- xuanji-agent 端 handler 保留 `params.files` 和顶层 `files` 双格式兼容
- WuKongIM CMD 消息使用 `dawn.create_task` 命令名
