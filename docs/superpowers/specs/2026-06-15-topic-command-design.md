# `/topic` 跨渠道话题绑定设计

> 日期：2026-06-15
> 状态：✅ 设计已与维护方确认，待 implementation plan
> 关联：
> - 依赖 [DawnIM 多话题映射](2026-06-14-dawn-im-multi-topic-design.md)（topic→thread_ts、`unified_<master>_<topic>` session key）
> - 依赖既有身份绑定（`/bind` / `/unbind` + `IdentityResolver`）
> - 与 `resolve_session_key` Phase 0 修复（保留 topic 后缀于统一 session key）联动

## 1. 背景与问题

`master_channel` 概念允许多个渠道的同一 superuser 通过 `/bind` 把身份归并到一个 `master_id`，使其会话历史与记忆跨渠道共享。结合多话题映射，DawnIM (master) 上 superuser 可以同时维护多条话题线（`unified_<master>_<topic_X>`）。

**问题**：无原生 topic 字段的渠道（feishu / wecom_ws / telegram 等）无法表达"我想把这条消息归入 master 的某个 topic"，导致：

- superuser 在 feishu 上发的消息只能落到 `unified_<master>`（无 topic）
- 想继续 dawnim 上某个 topic 的话题，只能切回 dawnim 客户端

## 2. 设计目标

- 在不支持原生 topic 的渠道上，superuser 可用 `/topic` 命令把当前 (channel, sender) 绑定到 master 的某个 topic
- 绑定期间该 (channel, sender) 的消息自动归入 `unified_<master>_<topic>` session
- 绑定独立于身份绑定，per-(channel, sender) 隔离，互不影响
- 绑定持久化（重启恢复）
- 工具栈可见绑定后的 effective topic（通过 `ChannelOrigin.topic`）

## 3. 非目标与前提假设

非目标：

- **不**在 master 渠道（DawnIM）上实现 `/topic` 命令——DawnIM 客户端自行拦截 `/topic`，ZeroClaw 收不到
- **不**改变回复路由——回复依然送回消息原始渠道，仅会话/记忆 namespace 跨渠道合并
- **不**对绑定加 TTL——持久化直至 `/topic reset`
- **不**实现 `/unbind` 级联清除 `topic_binding`——孤立 binding 在 identity 不存在时自然失效
- **不**改变 cron / tool 主动发送的消息路径——`topic_binding` 仅作用于入站用户消息

前提假设：

- **master 渠道是 DawnIM 类型**——topic 清单查询依赖 `dawn_im_<alias>` SqliteMemory。如果未来 master 改为其他类型渠道，本设计的 §5.6 需要重新评估
- **`master_channel` 配置值是有效的 channel_ref**——其 alias 部分可通过 `:` 分隔提取（与 ChannelRef 现有约定一致）

## 4. 用户场景

| 场景 | 设置 | 行为 |
|------|------|------|
| **S1：feishu 独立** | master=dawnim，feishu 已 `/bind` 但未 `/topic` | feishu 消息 → `unified_<master>`（与 dawnim 主线合并，但与 dawnim 各 topic 隔离） |
| **S2：feishu 绑 topic B** | master=dawnim，feishu 已 `/bind` + `/topic B` | feishu 消息 → `unified_<master>_B`，与 dawnim 上 topic B 共享历史与记忆 |
| **S3：dawnim 切 topic A，feishu 绑 topic B** | 同上 | dawnim 消息 → `unified_<master>_A`；feishu 消息 → `unified_<master>_B`。互不干扰 |
| **S4：u_alice 与 u_bob 分别绑** | 都在 feishu 上 | per-(channel, sender) key，互相独立 |

## 5. 核心机制

### 5.1 effective_topic 解析

```rust
fn resolve_effective_topic(
    msg: &ChannelMessage,
    channel_ref: &str,
    master_channel_ref: Option<&str>,
    topic_binding: Option<&TopicBindingRegistry>,
) -> Option<String> {
    if master_channel_ref == Some(channel_ref) {
        // master：永远信 thread_ts，binding 不参与
        return msg.thread_ts.clone();
    }
    // slave：thread_ts 优先，binding 兜底
    msg.thread_ts.clone().or_else(|| {
        topic_binding.and_then(|b| b.get(channel_ref, &msg.sender))
    })
}
```

**关键规则**：

- master 渠道完全忽略 binding（即使存在也无效）。理由：master 客户端原生提供 thread_ts，binding 没有意义
- 其他渠道：thread_ts（如有）优先于 binding，binding 仅在无原生 topic 时填空
- `binding` 无则返回 `None`，行为完全等同于现状

### 5.2 与 session key / ChannelOrigin 联动

`process_channel_message_body` 中计算 `effective_topic = resolve_effective_topic(...)` 一次，下发两处：

1. **session_key**：把 `msg.thread_ts` 临时替换为 `effective_topic` 后调用 `resolve_session_key`（或新增 `resolve_session_key_with_topic` 接受 `effective_topic` 入参）
   - feishu (bound B) → `unified_<master>_B`
   - feishu (无 binding) → `unified_<master>`（Phase 0 行为）
2. **ChannelOrigin.topic**：填 `effective_topic`，让工具栈看到绑定后的逻辑话题

### 5.3 TopicBindingRegistry 数据结构

```rust
pub struct BindingKey {
    pub channel_ref: String,
    pub sender: String,
}

pub struct TopicBindingRegistry {
    bindings: RwLock<HashMap<BindingKey, String>>, // → topic_id
    persist_path: PathBuf,                          // {data_dir}/sessions/topic_binding.json
}

impl TopicBindingRegistry {
    pub fn load(data_dir: &Path) -> io::Result<Self>;
    pub fn get(&self, channel_ref: &str, sender: &str) -> Option<String>;
    pub fn set(&self, channel_ref: &str, sender: &str, topic_id: &str) -> io::Result<()>;
    pub fn clear(&self, channel_ref: &str, sender: &str) -> io::Result<bool>;
}
```

- 内存 `HashMap`，启动时从 JSON 加载
- 每次 set/clear 后**同步**重写 JSON（小文件，开销可忽略；同步写换简单性，避免并发持久化竞态）
- 写失败仅日志告警，不影响内存状态（best-effort 持久化；下次成功写入时一致性恢复）
- **持久化序列化**：手动序列化（不依赖 serde 派生），key 拼接为 `"{channel_ref}|{sender}"`。读时严格按首个 `|` 拆分，多于一个 `|` 的条目视为损坏并跳过（warning log）

### 5.4 持久化文件

路径：`{data_dir}/sessions/topic_binding.json`

格式（扁平 map，key 为 `"<channel_ref>|<sender>"`）：

```json
{
  "feishu_v2:guild_xyz:u_alice": "db_lock",
  "wecom_ws:default:u_bob": "migrations"
}
```

选择分隔符 `|`：channel_ref 已含 `:`，sender 一般无 `|`；避免再嵌套 JSON 对象。

不存在或解析失败：视为空 map（不报错，仅日志），首次写时创建。

### 5.5 命令解析与处理

`ChannelRuntimeCommand` 新增 variant：

```rust
enum ChannelRuntimeCommand {
    // ...既有...
    Topic(TopicAction),
}

enum TopicAction {
    Help,                  // `/topic`
    List,                  // `/topic list`
    Reset,                 // `/topic reset`
    Set(String),           // `/topic <id>`
}
```

`parse_runtime_command`：

- `/topic` (no arg) → `Topic(Help)`
- `/topic list` → `Topic(List)`
- `/topic reset` → `Topic(Reset)`
- `/topic <id>` → `Topic(Set(id))`
- 解析规则：先尝试匹配 `list` / `reset` / `help` 关键字（不进入 Set 分支），其余视为 topic id。**因此 topic 名不能等于这三个保留词**——若用户试图设置，会被解析为 List/Reset/Help 而非绑定操作，无明确报错；记录为已知限制
- `/topic <id> <extra>`（带多余 token） → 视为非法参数，返回 Help

`handle_runtime_command_if_needed::Topic` 分支：

1. **权限检查**：`identity.resolver.resolve(channel_ref, sender, is_master)`
   - 返回 `None` → 回复"`/topic` 仅 superuser 可用"
   - 返回 `Some(master_id)` → 继续
2. 分发 action：
   - `Help` → 回复用法说明
   - `List` → 查 `master_channel` 对应的 DawnIM SqliteMemory，列出 `unified_<master_id>_*` 的 topic 集合 + 当前绑定标记
   - `Set(id)` → 验证 `id` 在 List 结果中（不在 → 拒绝），通过则 `topic_binding.set(...)` + 回复确认
   - `Reset` → `topic_binding.clear(...)` + 回复确认

### 5.6 topic 清单查询

在 `crates/zeroclaw-memory/src/sqlite.rs` 加：

```rust
impl SqliteMemory {
    /// 列出形如 `unified_<master_id>_<topic>` 的 session_id 中所有不同的 topic 后缀
    pub fn list_unified_topics(&self, master_id: &str) -> Result<Vec<String>>;
}
```

SQL：

```sql
SELECT DISTINCT session_id
  FROM memory_entries
 WHERE session_id LIKE 'unified_' || ? || '_%'
```

Rust 端解析：`session_id.strip_prefix(&format!("unified_{master_id}_"))` 提取 topic 字符串。

**注意**：`unified_<master_id>`（无 topic 后缀）不在结果中，符合"列出 topic"的语义。

数据源选择：用 `dawn_im_<alias>` 那个 SqliteMemory 实例。**alias 解析**：`master_channel` 形如 `"dawn_im_v2:work"`，按 `:` 分隔取最后一段作为 alias（与现有 ChannelRef 约定一致），打开 `{data_dir}/memory/dawn_im_<alias>.db`。

**不**用 conversation history store——后者可能未配置持久化。memory 表是 superuser 经长期对话天然积累的索引。

**降级行为**：若 master 渠道未对应 SqliteMemory 实例（如配置变更后未重启），`/topic list` 返回空列表 + 警告日志，不阻塞 `/topic <id>`（验证步骤会因列表为空而拒绝设置）。

### 5.7 启动期装配

`make_topic_binding_registry(data_dir)` 工厂位于 `crates/zeroclaw-infra/src/lib.rs`，与 `make_identity_store` 并列。

`ChannelRuntimeContext` 加：

```rust
topic_binding: Option<Arc<TopicBindingRegistry>>,
```

仅在 `config.channels.master_channel` 非空时构造（与 identity_store 启用条件一致），否则 `None`。

每个 agent 的 `ChannelRuntimeContext` 共享同一 `Arc<TopicBindingRegistry>`（同 `shared_identity` 模式）。

## 6. UI / 回复格式

所有回复用中文（与 `/bind` 一致）。

`/topic` (Help)：

```
用法：
  /topic list        查看 master 渠道上的所有话题
  /topic <名称>      把当前渠道绑定到指定话题
  /topic reset       解除绑定，恢复独立会话

仅 superuser 可用。
```

`/topic list`（有内容）：

```
你在 dawnim.work 的话题（共 3 个）：
  • db_lock         ← 当前绑定
  • migrations
  • casual_chat

用法：/topic <名称> 绑定，/topic reset 解绑
```

`/topic list`（空）：

```
你在 dawnim.work 尚无任何话题。请先在 dawnim 客户端创建话题并发送消息。
```

`/topic <id>` 验证失败：

```
话题 "<id>" 不存在。运行 /topic list 查看可用话题。
```

`/topic <id>` 成功：

```
已绑定到话题 "<id>"。本渠道后续消息将归入该话题的对话历史。
```

`/topic <id>` 已绑定到同 id：

```
已绑定到话题 "<id>"（无变化）。
```

`/topic reset` 成功：

```
已清除话题绑定。本渠道恢复独立会话。
```

`/topic reset` 未绑定：

```
当前没有话题绑定。
```

非 superuser：

```
/topic 仅 superuser 可用。
```

## 7. Edge Cases

| 场景 | 行为 |
|------|------|
| master 渠道收到 `/topic`（理论上不会发生） | 按普通命令处理；binding 即使被设置也不会生效（5.1 规则） |
| `/bind` 未做就 `/topic` | identity.resolve 返回 None → "仅 superuser 可用" |
| `/unbind` 后 `topic_binding` 残留 | 残留 binding 不影响：下次入站消息 identity 返回 None，session_key 走 base 路径（不进 unified namespace），binding 自然失效 |
| topic 绑定后该 topic 在 dawnim 客户端被删除 | ZeroClaw 端 session 仍存在，binding 继续有效；ZeroClaw 不感知 dawnim 端 topic 生命周期 |
| Cron / Tool 发起的消息 | 不走 `process_channel_message_body` 的 ChannelOrigin 路径，binding 不查 |
| 同 superuser 在 feishu 和 wecom 各 `/topic` 不同 id | per-(channel, sender) 各自独立 |
| 持久化文件损坏 | 启动时解析失败 → 视为空 map + 警告日志；不阻塞启动 |
| 持久化写失败 | 内存 binding 仍生效；告警日志；下次成功写时恢复一致 |
| 重启后 binding 恢复 | 从 JSON 加载，行为延续 |
| `/admin/reload` | 重新加载 JSON（内存状态重建） |
| topic 名恰为保留词 (`list` / `reset` / `help`) | 已知限制：用户无法绑定到这三个名字的 topic。文档中说明 |
| 入站消息 sender 含 `|` 分隔符 | 序列化时按 `"{channel_ref}|{sender}"` 拼接；首个 `|` 拆分，sender 内若含 `|` 会破坏读取。**约束**：sender ID 不允许含 `|`（与现有 channel_ref 约束一致） |

## 8. 安全考量

- **权限边界**：`/topic` 强依赖 `IdentityResolver` 的 superuser 判定——非 superuser 无法触发任何修改
- **跨用户隔离**：per-(channel, sender) key 确保 u_alice 的 binding 不影响 u_bob
- **跨 master_id 隔离**：`list_unified_topics(master_id)` 只返回该 master_id 的 topic，u_alice 看不到 u_bob 的
- **持久化文件权限**：与 identity.db 同目录，inherit data_dir 权限（部署时 superuser owned）

## 9. 文件改动概览

| 文件 | 改动 | 估算 LOC |
|------|------|---------|
| `crates/zeroclaw-infra/src/topic_binding.rs`（新） | `TopicBindingRegistry` 实现 + JSON IO | ~120 |
| `crates/zeroclaw-infra/src/lib.rs` | `make_topic_binding_registry` factory | ~20 |
| `crates/zeroclaw-memory/src/sqlite.rs` | `list_unified_topics` 方法 | ~30 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | `ChannelRuntimeCommand::Topic` + parse + handle + `resolve_effective_topic` 引入 + `ChannelRuntimeContext.topic_binding` 字段 + 启动期装配 | ~250 |
| 测试 | unit + integration | ~200 |
| **合计** | | **~620** |

## 10. 测试计划

### 10.1 Unit

- `TopicBindingRegistry`：get/set/clear 基本行为
- `TopicBindingRegistry`：set→load→get round-trip（持久化）
- `TopicBindingRegistry`：corrupt JSON → 空 map + warning
- `resolve_effective_topic`：matrix 覆盖
  - master + thread_ts → thread_ts
  - master + thread_ts + binding → thread_ts（binding 忽略）
  - slave + thread_ts → thread_ts
  - slave + binding → binding
  - slave + thread_ts + binding → thread_ts
  - slave + 都无 → None
- `parse_runtime_command`：`/topic` / `/topic list` / `/topic reset` / `/topic foo` / `/topic foo bar`（多余 token 视为非法）
- `SqliteMemory::list_unified_topics`：seed session_ids，验证返回集合与顺序

### 10.2 Integration

- feishu user `/bind` + `/topic <id>` → 后续消息 session_key 命中 `unified_<master>_<id>`
- feishu user `/topic reset` → 后续消息 session_key 回落到 `unified_<master>`
- 同 master_id 下 dawnim topic A + feishu binding B → 两者各自独立 session
- 两个 superuser 在 feishu 各自 `/topic` → 互不影响
- 非 superuser `/topic` → 拒绝
- 重启进程（模拟）→ binding 从 JSON 恢复

### 10.3 手测项

- DawnIM 客户端拦截 `/topic` 行为验证（确认 ZeroClaw 端确实未收到 master 上的 `/topic` 消息）

## 11. 迁移与回退

- **无 SQLite migration**——新文件 + 既有表的新方法
- **无 config 变更**——`master_channel` 已存在
- **回退**：删除 `topic_binding.json` 即可恢复原行为；代码层 `topic_binding: None` 的 ChannelRuntimeContext 走原路径
- **依赖**：身份绑定（`/bind`）必须先行——`/topic` 权限检查复用 `IdentityResolver::resolve`

## 12. 后续工作（非本期）

- 在 `/topic list` 中追加 topic 元数据（如最近活动时间、消息数）——需要扩展 `list_unified_topics` SQL
- topic 自动归类 / 类型推断（独立后续工作，见 [DawnIM 多话题设计](2026-06-14-dawn-im-multi-topic-design.md) §3）
- 跨 agent 共享 binding（当前 binding registry 是 process 全局，已天然跨 agent；只是确认这是预期）
