# ZeroClaw Session 机制与会话工具开发文档

> 面向后续开发者的内部参考。描述 ZeroClaw 中"会话(session)"的定义、标识生成、
> 分层存储、两种持久化后端、生命周期以及 `sessions_*` 系列工具的实现细节。
> 所有引用均标注 `文件:行号`,便于直接跳转源码核对。

---

## 1. 核心概念:session 是隐式的

ZeroClaw **没有显式的 "Session 对象" 或 "创建 session" 的 API**。会话是一个
**由 `session_key` 派生的隐式概念**:只要某个 key 第一次产生消息,对应的 session
就"存在"了;消息被清空或文件/行被删除,session 就"消失"。这是一种 *惰性
upsert* 模型。

一个 session 在两层各有体现:

- **内存层**:进程内的 LRU 缓存(`ConversationHistoryMap`),保存最近活跃 sender 的对话历史。
- **持久层**:落盘的 `SessionBackend`(JSONL 文件或 SQLite 数据库),用于重启后恢复上下文。

二者通过同一个 `session_key` 关联。

---

## 2. Session Key(会话标识)

### 2.1 channel 消息的 key 生成

入站 channel 消息的 session_key 由 `conversation_history_key()` 计算
(`crates/zeroclaw-channels/src/orchestrator/mod.rs:470`):

```
channel_scope + "_" + reply_target + ["_" + thread_ts] + "_" + sender
```

| 组成部分 | 含义 | 隔离作用 |
|---|---|---|
| `channel_scope` | `channel类型` 或 `channel类型.alias`(如 `discord.clamps`) | 同平台不同 bot 互不共享历史 |
| `reply_target` | 平台侧房间/频道/会话 ID | 同 channel 内不同房间隔离 |
| `thread_ts` | 可选的线程/话题 ID | 论坛群内按话题隔离 |
| `sender` | 发送者 ID | 同房间内不同用户隔离 |

特殊处理:

- `wecom_ws` 渠道只用 `channel_scope_reply_target`,不附加 sender
  (`orchestrator/mod.rs:478`)。
- Matrix:当 `thread_ts == msg.id`(根事件自锚定)时忽略 thread 维度,
  否则每条顶层消息都会变成一个全新 session(`orchestrator/mod.rs:491`)。

> **隔离粒度小结:`哪个 bot × 哪个房间 × 哪个话题 × 哪个人`。四要素任一不同 →
> 不同 session。因此跨 channel 的对话历史天然隔离,互相获取不到上下文。**

> ⚠️ 注意:仓库内存在两份 `conversation_history_key`。
> `crates/zeroclaw-channels/src/util.rs:222` 是旧的简化版(无 `channel_alias`
> 作用域);**实际生效的是 `orchestrator/mod.rs:470` 这一份**(带 alias 作用域)。
> 新代码应以 orchestrator 版本为准。

### 2.2 key 规范化(sanitize)

所有 key 在落盘前都要经过 `sanitize_session_key()`
(`crates/zeroclaw-api/src/session_keys.rs:16`):把 `[A-Za-z0-9_-]` 之外的字符
全部替换为 `_`,且**幂等**。

原因:同一个 key 要同时作为 ① 运行时 HashMap 键、② JSONL 文件名、
③ memory 后端的 `session_id` 列,三处必须一致,否则重启后 hydration 会在
不同名字下读写同一会话。调用方在构造 key 时必须预先 sanitize。

### 2.3 其它来源的 key 前缀

| 来源 | key 形态 | 位置 |
|---|---|---|
| Channel 消息 | `channel.alias_room_[thread_]sender` | `orchestrator/mod.rs:470` |
| RPC / TUI | `rpc_{session_id}` | `crates/zeroclaw-runtime/src/rpc/dispatch.rs:866` |
| Gateway 仪表盘 | `gw_{session_id}` | 见 `sessions.rs` 工具错误提示 |
| Cron 定时任务 | scheduler 合成 | `crates/zeroclaw-runtime/src/cron/scheduler.rs:617` |

---

## 3. 分层存储架构

### 3.1 内存层:LRU 缓存

```rust
// crates/zeroclaw-channels/src/orchestrator/mod.rs:202
type ConversationHistoryMap = Arc<Mutex<lru::LruCache<String, Vec<ChatMessage>>>>;
```

约束常量(`orchestrator/mod.rs:206-208`):

- `MAX_CONVERSATION_SENDERS = 1000`:内存中最多保留 1000 个 sender 的历史,
  超出时按 LRU 淘汰最久未访问者。
- `MAX_CHANNEL_HISTORY = 50`:每个 sender 在内存中最多保留 50 条消息。
- `PROACTIVE_CONTEXT_BUDGET_CHARS = 400_000`(`mod.rs:250`):发送给模型前的
  主动上下文预算,超出则丢弃更早的轮次,防止 context-window 溢出。

另有 `PendingNewSessionSet`(`mod.rs:204`):记录发过 `/new` 的 sender,
其下一条消息会强制重建 prompt。

### 3.2 持久层:`SessionBackend` trait

定义于 `crates/zeroclaw-infra/src/session_backend.rs:75`。实现必须 `Send + Sync`。
核心方法(节选):

| 方法 | 作用 |
|---|---|
| `load(key) -> Vec<ChatMessage>` | 读取某 session 全部消息,不存在返回空 |
| `load_with_timestamps(key)` | 同上但带 `created_at`(SQLite 有,JSONL 为 None) |
| `append(key, msg)` | 追加一条消息(**首次追加即隐式创建 session**) |
| `remove_last(key) -> bool` | 删最后一条(回滚用) |
| `update_last(key, msg)` | 改最后一条(流式增量持久化) |
| `clear_messages(key) -> usize` | **清空消息,保留 session 空壳** |
| `delete_session(key) -> bool` | **删除整个 session** |
| `session_exists(key) -> bool` | 廉价存在性探测(#7126) |
| `list_sessions() -> Vec<String>` | 列出所有 key |
| `list_sessions_with_metadata()` | 列出 key + 元数据 |
| `set_session_context(key, ctx)` | 记录 channel/room/sender 路由列(SQLite) |
| `set_session_agent_alias(key, alias)` | 记录归属 agent(多 agent 归属) |
| `set_session_state(key, state, turn_id)` | 记录运行状态 idle/running/error |
| `cleanup_stale(ttl_hours) -> usize` | 按 TTL 清理陈旧 session |
| `search(query)` | 关键字搜索(SQLite 用 FTS5) |

`SessionMetadata`(`session_backend.rs:12`)字段:`key / name / created_at /
last_activity / message_count / agent_alias / channel_id / room_id / sender_id`。
其中后三个是结构化路由列,便于仪表盘按平台属性过滤,无需反解 session_key。

---

## 4. 两种后端实现

### 4.1 JSONL —— `SessionStore`

- 位置:`crates/zeroclaw-infra/src/session_store.rs`
- 存储:每个 session 一个 append-only 文件 `{workspace}/sessions/{sanitized_key}.jsonl`,
  一行一条消息的 JSON,永不改写旧行。
- `list_sessions()`:扫描 `sessions/` 目录下的 `*.jsonl` 文件名。
- `clear_messages`:`rewrite(key, &[])` 把文件截断为空,**文件保留**(key 仍可列出)。
- `delete_session`:`remove_file` 删除整个文件。
- 没有元数据/全文检索;`last_activity` 用文件 mtime 近似
  (`session_store.rs:171` 覆盖了 trait 默认实现)。

### 4.2 SQLite —— `SqliteSessionBackend`(默认)

- 位置:`crates/zeroclaw-infra/src/session_sqlite.rs`
- 存储:`{workspace}/sessions/sessions.db`,WAL 模式,FTS5 全文检索,支持 TTL 清理。
- 模块注释自述:*"Designed as the default backend, replacing JSONL for new installations."*

### 4.3 后端选择与自动迁移

`make_session_backend()`(`crates/zeroclaw-infra/src/lib.rs:30`):

```rust
match backend {
    "jsonl"  => SessionStore::new(workspace_dir),
    "sqlite" => open_sqlite_with_jsonl_import(workspace_dir),
    other    => /* 警告 + 回退 sqlite */ open_sqlite_with_jsonl_import(workspace_dir),
}
```

- **默认值是 `"sqlite"`**(`crates/zeroclaw-config/src/schema.rs:11061`
  `default_session_backend()` 返回 `"sqlite"`)。
- 未知值会打 WARN 并回退到 sqlite,不会报错。
- **JSONL → SQLite 自动迁移**:首次以 sqlite 打开时,会导入遗留的
  `sessions/*.jsonl`,并把原文件重命名为 `*.jsonl.migrated`(保留以便回滚),
  迁移路径出错只记录日志不阻塞启动(`lib.rs:54` 注释,`open_sqlite_with_jsonl_import`)。

> 💡 **常见困惑:"找不到 jsonl 文件"。**
> 因为新安装默认用 SQLite,消息写入 `{workspace}/sessions/sessions.db`,
> **根本不会产生 `.jsonl` 文件**。这与具体渠道(如 dawn_im)无关 —— 所有走
> orchestrator 的渠道都共用同一后端。要查看历史,请用 SQLite 客户端打开该 db,
> 或临时把 `session_backend` 切回 `"jsonl"`。

---

## 5. SQLite 表结构

建表见 `session_sqlite.rs:39-77`,随后有若干 `ALTER TABLE` 渐进式迁移。

### 表 `sessions`(消息行)

```sql
id          INTEGER PRIMARY KEY AUTOINCREMENT
session_key TEXT NOT NULL
role        TEXT NOT NULL
content     TEXT NOT NULL
created_at  TEXT NOT NULL
-- 索引:idx_sessions_key(session_key), idx_sessions_key_id(session_key, id)
```

### 表 `session_metadata`(一行一个 session)

```sql
session_key   TEXT PRIMARY KEY
created_at    TEXT NOT NULL
last_activity TEXT NOT NULL
message_count INTEGER NOT NULL DEFAULT 0
name          TEXT                       -- 迁移新增
state         TEXT NOT NULL DEFAULT 'idle'  -- 迁移新增
turn_id       TEXT                       -- 迁移新增
turn_started_at TEXT                     -- 迁移新增
agent_alias   TEXT                       -- 迁移新增(多 agent 归属),带索引
channel_id    TEXT                       -- 迁移新增(结构化路由),带索引
room_id       TEXT                       -- 迁移新增,带索引
sender_id     TEXT                       -- 迁移新增,带索引
```

迁移采用"检查 `pragma_table_info` 是否已有该列,无则 `ALTER TABLE ADD COLUMN`"
的幂等方式(`session_sqlite.rs:79-177`),对旧库向后兼容,全部新增列可空。

### 虚表 `sessions_fts`(FTS5 全文索引)

`content=sessions, content_rowid=id` 的外部内容索引,由三个触发器
(`sessions_ai` / `sessions_ad` / `sessions_au`,`session_sqlite.rs:62-75`)
在 `sessions` 表 INSERT/DELETE/UPDATE 时自动同步。`delete_session` 删消息行时,
FTS 由 `sessions_ad` 触发器自动清理。

---

## 6. 配置

字段定义于 `crates/zeroclaw-config/src/schema.rs`:

| 字段 | 默认 | 说明 |
|---|---|---|
| `channels.session_persistence` | `true` | 是否启用持久化 |
| `channels.session_backend` | `"sqlite"` | `"sqlite"`(默认)或 `"jsonl"`(legacy) |
| `channels.session_ttl_hours` | `0` | 自动归档/清理早于该小时数的陈旧 session,`0` 表示禁用 |

`config.toml` 示例:

```toml
[channels]
session_persistence = true
session_backend = "sqlite"
session_ttl_hours = 0
```

实际数据库路径示例(workspace 由 `data_dir` 决定):

```
{workspace}/sessions/sessions.db          # 主库
{workspace}/sessions/archive/sessions.db  # 归档
{workspace}/sessions/*.jsonl.migrated     # 迁移后保留的 legacy 文件
```

装配入口:daemon 在启动时调用 `make_session_backend(workspace_dir,
&config.channels.session_backend)`(`crates/zeroclaw-runtime/src/daemon/mod.rs:392`),
得到的 `Arc<dyn SessionBackend>` 注入 orchestrator 与 RPC context。

---

## 7. 生命周期

### 7.1 创建(惰性)

无显式创建。第一条匹配某 key 的消息到达时:

1. 内存:`load` 返回空 → 该 key 进入 LRU 缓存;
2. 磁盘:`append` 时 JSONL 用 `OpenOptions::create(true).append(true)` 建文件
   (`session_store.rs:60`),SQLite 插入 `sessions` 行并 upsert `session_metadata`。

### 7.2 重启恢复(hydration)

daemon 重启时,从持久层按最近活跃排序把 session 重新灌进 LRU 缓存,恢复对话上下文
(`session_store.rs:171` 的 `list_sessions_with_metadata` 覆盖即为支撑此排序而设)。

### 7.3 清空 vs 删除(关键区别)

| 操作 | 内存 LRU | 持久层(磁盘/DB) | session 条目 | 后续可列出? |
|---|:--:|:--:|:--:|:--:|
| `clear_messages` | 不主动动 | 清空消息内容 | **保留(空壳)** | 是 |
| `delete_session` | 不主动动 | 删除全部数据 | **整个删除** | 否 |
| `/new` 命令 | ✅ pop 清除 | 调 `delete_session` | 整个删除 | 否 |

两种后端在这两个操作上的语义**完全一致**(仅实现细节不同):

- `clear_messages`
  - JSONL:截断文件为空,文件保留;返回清空前消息数(`load().len()`)。
  - SQLite:`DELETE FROM sessions WHERE session_key`,保留 `session_metadata` 行
    并把 `message_count=0`、刷新 `last_activity`;返回 `conn.changes()`。
    `name` 等元数据保留(`session_sqlite.rs:478`,测试 `clear_messages_removes_rows_keeps_metadata`)。
- `delete_session`
  - JSONL:`remove_file`;返回文件是否存在。
  - SQLite:删 `sessions` + `session_metadata`,FTS 由触发器清理;返回 metadata 是否存在
    (`session_sqlite.rs:500`)。
- `session_exists`:JSONL 看文件存在性;SQLite 看 `session_metadata` 行
  —— 都与各自 `delete_session` 擦除的对象对齐(#7126)。

### 7.4 `/new` 命令(channel 内)

处理于 `orchestrator/mod.rs:2250`(`ChannelRuntimeCommand::NewSession`):

```rust
clear_sender_history(ctx, &sender_key);          // ① 内存:LRU pop
store.delete_session(&sender_key);               // ② 持久层:删除整个 session
mark_sender_for_new_session(ctx, &sender_key);   // ③ 标记下条消息重建 prompt
```

即 `/new` 是"内存 + 持久层"双清,且为**删除式**(非清空式)。每个渠道都可用,
无 model-switch 门控(`mod.rs:1056`)。

### 7.5 TTL 清理

若 `session_ttl_hours > 0`,后端的 `cleanup_stale(ttl_hours)` 会移除超过该时长
未活跃的 session(JSONL 默认 no-op,SQLite 实现按 `last_activity` 清理)。

---

## 8. 会话工具:`sessions_*`(供 agent 调用)

定义于 `crates/zeroclaw-tools/src/sessions.rs`。这 6 个工具面向 **LLM agent**,
用于 inter-agent / inter-session 通信,而非用户的 CLI 命令(但可通过
`zeroclaw agent -a <alias> -m "..."` 让 agent 间接调用)。

| 工具 | 作用 | 权限门控 | 归属校验 |
|---|---|---|---|
| `sessions_current` | 返回当前所在 session 的 key 与元数据 | 无 | 无 |
| `sessions_list` | 列出活跃 session(channel/最后活跃/消息数),参数 `limit` 默认 50 | 无 | 无 |
| `sessions_history` | 读指定 session 的最近 N 条(`session_id` 必填,`limit` 默认 20) | `Read` | 无 |
| `sessions_send` | 向指定 session 追加一条 user 消息(inter-agent 通信) | `Act` | 无(append) |
| `sessions_reset` | 清空指定 session 消息(可继续接收新消息) | `Act` | ✅ |
| `sessions_delete` | 永久删除指定 session(不可撤销) | `Act` | ✅ |

- `sessions_current` 通过 `TOOL_LOOP_SESSION_KEY` task-local 读取当前 session
  (`sessions.rs:482`),该 task-local 在 gateway / channel 的 agent turn 周围设置。
- 权限通过 `SecurityPolicy::enforce_tool_operation(op, name)` 检查,`op` 见上表
  (read_only 自治等级会拒绝 `Act` 类操作)。
- `session_id` 校验:`validate_session_id`(`sessions.rs:47`)要求非空且至少含一个
  字母数字字符。

### 8.1 归属作用域(多 agent 安全)

`sessions_reset` / `sessions_delete` 在 `for_agent(...)` 构造时携带
`SessionOwnershipScope { agent_alias, channel_ids }`,执行前调用
`scope.authorize(backend, session_id)`(`sessions.rs:98`)判定是否有权对该 session
做破坏性操作,规则按优先级:

1. session 不存在 → 放行(把 trim 后的 id 当作目标 key)。
2. session 存在但**无元数据** → **拒绝**(防止误删来历不明的会话)。
3. 元数据有 `agent_alias`:等于本 agent 才放行,否则报"owned by agent X"。
4. 否则看 `channel_id`:在本 agent 的 `channel_ids` 列表内才放行,否则报
   "belongs to channel X"。

若未携带 ownership_scope(`new(...)` 构造,非 `for_agent`),则不做归属校验,
直接解析已有 key 或退回原始 id。

### 8.2 已知瑕疵(供后续修复参考)

- 工具的 `session_id` 描述与示例使用 **双下划线** 约定(如 `telegram__user123`),
  且 `sessions_list` 用 `meta.key.split("__").next()` 提取 channel
  (`sessions.rs:193`);但 **实际 key 由 `conversation_history_key` 用单下划线
  拼接**(`channel.alias_room_sender`)。因此对真实渠道 key,`sessions_list` 的
  `channel=` 字段往往会显示成整个 key(`split("__")` 不命中)。这是展示层瑕疵,
  不影响存储与路由。

---

## 9. 各交互入口的 session 行为速查

| 入口 | 是否持久化历史 | session 切换方式 |
|---|---|---|
| Channel(Telegram/Discord/dawn_im 等) | 是(走 orchestrator → backend) | 自动按 `bot×房间×话题×人` 路由;`/new` 重置;不能手工切到别的 session |
| `zeroclaw agent -a x -m "..."`(单消息) | 否(每次独立) | —— |
| `zeroclaw agent -a x --session-state-file f.json` | 是(JSON 文件) | **换文件即换 session**(最接近"手工切换") |
| RPC / TUI | 是(`rpc_*` key) | 握手时确定 session_id |
| Gateway WebSocket | 是(`gw_*` key) | 一条连接对应一个 session |
| Cron | 是(scheduler 合成 key) | —— |

> 跨 channel 的对话历史相互隔离;若要让信息跨 channel 流转,应走 **memory**
> 子系统(全局,不按 channel 分键),而非依赖会话历史。

---

## 10. 关键源码索引

| 主题 | 位置 |
|---|---|
| key 生成(生效版) | `crates/zeroclaw-channels/src/orchestrator/mod.rs:470` |
| key 生成(旧简化版) | `crates/zeroclaw-channels/src/util.rs:222` |
| key sanitize | `crates/zeroclaw-api/src/session_keys.rs:16` |
| 内存 LRU 类型与常量 | `crates/zeroclaw-channels/src/orchestrator/mod.rs:202-250` |
| `/new` 处理 | `crates/zeroclaw-channels/src/orchestrator/mod.rs:2250` |
| `SessionBackend` trait | `crates/zeroclaw-infra/src/session_backend.rs:75` |
| JSONL 后端 | `crates/zeroclaw-infra/src/session_store.rs` |
| SQLite 后端 + schema | `crates/zeroclaw-infra/src/session_sqlite.rs` |
| 后端选择 + JSONL→SQLite 迁移 | `crates/zeroclaw-infra/src/lib.rs:30` |
| 配置字段与默认值 | `crates/zeroclaw-config/src/schema.rs:10726`、`:11061` |
| daemon 装配后端 | `crates/zeroclaw-runtime/src/daemon/mod.rs:392` |
| `sessions_*` 工具 | `crates/zeroclaw-tools/src/sessions.rs` |
| 归属校验 | `crates/zeroclaw-tools/src/sessions.rs:98` |

---

## 11. 开发注意事项

1. **构造 key 必须先 sanitize**,否则重启后 hydration 与运行时查找会用不同名字读写同一会话。
2. **新增 backend** 时,优先覆盖 `clear_messages` / `list_sessions_with_metadata` /
   `get_session_metadata` 等带性能/语义注解的默认实现(trait 默认多为 O(n) 或
   `Utc::now()` 占位,仅适合测试)。
3. **破坏性 session 操作**(reset/delete)在 agent 路径上务必通过 `for_agent` 携带
   ownership_scope,避免越权删除其它 agent / channel 的会话。
4. **不要假设存在 `.jsonl` 文件**:默认后端是 SQLite。排障时先确认
   `channels.session_backend` 的实际取值与 `sessions.db` 路径。
5. **`clear_messages` 不触碰内存 LRU**:若对一个内存中仍活跃的 channel session 调
   `sessions_reset`,会出现"磁盘已空、内存仍有旧历史"的短暂不一致,直到该 session
   被 `/new`、被 LRU 淘汰或 daemon 重启重新 hydrate。要彻底重置当前 channel 对话用 `/new`。
