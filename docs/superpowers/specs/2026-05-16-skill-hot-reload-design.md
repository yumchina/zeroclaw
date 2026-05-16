# ZeroClaw Skill 热加载设计文档

**日期**: 2026-05-16
**状态**: 设计阶段
**作者**: Claude + 用户协作

---

## Context

ZeroClaw 目前仅在启动时加载 skills，或在收到 `/admin/reload` 时重新加载整个 daemon。这意味着用户修改 skill 文件后需要手动触发 reload 或重启进程才能生效，影响开发体验。

本文档设计一个文件级热加载机制，监控 skills 目录变化并自动重新加载，无需重启 daemon。

---

## Requirements

| 需求 | 描述 |
|------|------|
| **实时性** | 检测到 skill 文件变化后自动触发重载 |
| **全量重载** | 重新扫描整个 skills 目录，重新注册所有 skill tools |
| **优雅过渡** | 当前对话轮次完成后再切换，避免中断正在进行的 tool 调用 |
| **并发安全** | 多个 agent 会话同时运行时安全更新 tools |
| **降级能力** | 文件监控不可用时降级到轮询模式 |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        ZeroClaw Daemon                               │
│                                                                      │
│  ┌─────────────────┐         ┌──────────────────────────────────┐  │
│  │  SkillWatcher   │         │      ToolRegistryManager         │  │
│  │  (notify task)  │────────▶│  (Arc<Vec<Box<dyn Tool>>>)       │  │
│  └─────────────────┘  swap   │  - atomic_swap(new_tools)        │  │
│       - 监控目录              │  - get_current()                 │  │
│       - 防抖 300ms            └────────────▲─────────────────────┘  │
│       - 触发重载                           │                        │
│                                    ┌───────┴────────┐             │
│                                    │                │             │
│                          ┌─────────▼─────┐  ┌─────▼──────────┐   │
│                          │  Agent Loop 1 │  │  Agent Loop N  │   │
│                          │  (克隆 Arc)   │  │  (克隆 Arc)    │   │
│                          └───────────────┘  └────────────────┘   │
└─────────────────────────────────────────────────────────────────────┘
```

### Core Components

1. **SkillWatcher**: 独立的 tokio 任务，监控 skills 目录
2. **ToolRegistryManager**: 持有共享的 `Arc<Vec<Box<dyn Tool>>>`，提供原子替换
3. **Agent Loops**: 每个会话克隆 Arc，自然获取最新 tools

---

## Data Flow

```
skill 文件变更
    │
    ▼
notify::Event (Write/Remove/Rename)
    │
    ▼
SkillWatcher::debounce_tx (300ms 防抖)
    │
    ▼
load_skills_with_config()  // 现有函数
    │
    ▼
register_skill_tools()     // 现有函数
    │
    ▼
ToolRegistryManager::atomic_swap(new_tools)
    │
    ▼
广播 "skills:reloaded" 事件
```

---

## Key Interfaces

### SkillWatcher (新文件)

**文件**: `crates/zeroclaw-runtime/src/skills/watcher.rs`

```rust
use notify::{RecursiveMode, Watcher, recommended_watcher};
use tokio::sync::mpsc;
use std::path::PathBuf;

pub struct SkillWatcher {
    skills_dir: PathBuf,
    reload_tx: mpsc::Sender<ReloadRequest>,
}

pub struct ReloadRequest {
    pub force: bool,
}

impl SkillWatcher {
    pub fn spawn(skills_dir: PathBuf) -> JoinHandle<Result<()>> {
        tokio::spawn(async move {
            let (debounce_tx, mut debounce_rx) = mpsc::channel(1);
            let mut watcher = recommended_watcher(move |res| {
                // 处理 notify 事件，发送到防抖 channel
            })?;

            watcher.watch(&skills_dir, RecursiveMode::Recursive)?;

            // 防抖循环
            let mut last_event = Instant::now();
            const DEBOUNCE_MS: u64 = 300;

            while let Some(_) = debounce_rx.recv().await {
                let now = Instant::now();
                let elapsed = now.duration_since(last_event).as_millis() as u64;

                if elapsed >= DEBOUNCE_MS {
                    if let Err(_) = reload_tx.send(ReloadRequest { force: false }).await {
                        break; // Channel closed
                    }
                    last_event = now;
                }
            }

            Ok(())
        })
    }
}
```

### ToolRegistryManager (新文件)

**文件**: `crates/zeroclaw-runtime/src/tools/registry_manager.rs`

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct ToolRegistryManager {
    inner: Arc<Vec<Box<dyn Tool>>>,
    version: AtomicU64,
}

impl ToolRegistryManager {
    pub fn new(tools: Vec<Box<dyn Tool>>) -> Self {
        Self {
            inner: Arc::new(tools),
            version: AtomicU64::new(1),
        }
    }

    /// 原子性地替换 tools，返回新版本号
    pub fn atomic_swap(&self, new_tools: Vec<Box<dyn Tool>>) -> u64 {
        let new_arc = Arc::new(new_tools);
        let old_arc = std::mem::replace(&mut self.inner, new_arc);
        let new_version = self.version.fetch_add(1, Ordering::SeqCst) + 1;

        // 旧 Arc 在所有活跃引用释放后自动 drop
        tracing::info!("Tool registry swapped to version {}", new_version);
        new_version
    }

    pub fn get_current(&self) -> Arc<Vec<Box<dyn Tool>>> {
        Arc::clone(&self.inner)
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::SeqCst)
    }
}
```

### Agent Loop 修改

**文件**: `crates/zeroclaw-runtime/src/agent/loop_.rs`

```rust
pub struct AgentState {
    // ...
    tools_mgr: Arc<ToolRegistryManager>,  // 替代 tools_registry
}

// 在 run() 和 process_message() 中
let tools_mgr = Arc::new(ToolRegistryManager::new(tools));

// 每次迭代获取当前 tools
let current_tools = tools_mgr.get_current();
```

---

## Error Handling

| 场景 | 处理方式 |
|------|----------|
| skill 文件格式错误 | 跳过该 skill，记录警告日志 |
| skills 目录不存在 | 使用空 Vec，记录警告 |
| 监控失败 (notify 错误) | 降级为轮询模式 (每 30s) |
| 重载失败 (parse 错误) | 保持旧 registry，发送错误事件 |

### 事件定义

```rust
pub enum SkillReloadEvent {
    Started,
    Success { count: usize, version: u64 },
    Failed { error: String },
    Skipped { skill: String, reason: String },
}
```

---

## Configuration

**新增配置** (`config.toml`):

```toml
[skills.hot_reload]
enabled = true
debounce_ms = 300
watch_mode = "notify"  # "notify" | "poll" | "off"
poll_interval_secs = 30  # 降级轮询间隔
```

---

## Testing Plan

| 测试类型 | 测试内容 |
|----------|----------|
| 单元测试 | `SkillWatcher` 防抖逻辑 |
| 单元测试 | `ToolRegistryManager::atomic_swap` 并发安全 |
| 集成测试 | 创建 skill 文件 → 验证重载触发 |
| 集成测试 | 修改 skill → 验证新 tool 生效 |
| 集成测试 | 删除 skill → 验证 tool 移除 |
| 压力测试 | 频繁文件修改 → 验证防抖和并发 |
| 回归测试 | 确保现有 `/admin/reload` 仍正常 |

---

## Implementation Phases

### Phase 1: ToolRegistryManager
- 实现 `ToolRegistryManager` 结构体
- 单元测试并发安全性
- 集成到 agent loop

### Phase 2: SkillWatcher
- 实现 `SkillWatcher` 文件监控
- 实现防抖逻辑
- 添加降级轮询模式

### Phase 3: Integration
- 连接 SkillWatcher 和 ToolRegistryManager
- 添加配置项
- 实现事件广播

### Phase 4: Testing
- 单元测试覆盖
- 集成测试
- 性能测试

---

## Files to Modify

| 文件 | 修改类型 |
|------|----------|
| `crates/zeroclaw-runtime/src/tools/registry_manager.rs` | 新增 |
| `crates/zeroclaw-runtime/src/skills/watcher.rs` | 新增 |
| `crates/zeroclaw-runtime/src/skills/mod.rs` | 修改 (集成 watcher) |
| `crates/zeroclaw-runtime/src/agent/loop_.rs` | 修改 (使用 ToolRegistryManager) |
| `crates/zeroclaw-runtime/src/agent/agent.rs` | 修改 (使用 ToolRegistryManager) |
| `crates/zeroclaw-config/src/schema.rs` | 修改 (新增配置) |
| `crates/zeroclaw-gateway/src/lib.rs` | 修改 (可选: 添加 `/admin/skills/reload` 端点) |

---

## Dependencies

**新增依赖** (`Cargo.toml`):

```toml
[dependencies]
notify = "7"        # 文件监控
```

---

## Performance Considerations

- `Arc::swap` 是无锁操作，开销极小 (~原子指令)
- 防抖确保短时间内多次修改只触发一次重载
- 每个 agent 会话克隆 Arc (指针复制，非深拷贝)
- 旧 Arc 在所有活跃引用释放后自动 drop

---

## Future Enhancements

- 支持 skill 依赖关系（重载顺序）
- 增量重载（仅加载变更的 skill）
- Webhook 通知外部系统
- 前端 UI 显示 skill 版本和重载历史

---

## Usage Example

### 启用热加载

在 `config.toml` 中启用技能热加载：

```toml
[skills.hot_reload]
enabled = true
debounce_ms = 300
watch_mode = "notify"  # "notify" | "poll" | "off"
poll_interval_secs = 30  # 轮询模式下的检测间隔（秒）
```

### 配置说明

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `enabled` | `false` | 是否启用热加载 |
| `debounce_ms` | `300` | 防抖延迟（毫秒），防止频繁重载 |
| `watch_mode` | `"notify"` | 监控模式：`notify`(文件系统事件)、`poll`(轮询)、`off`(禁用) |
| `poll_interval_secs` | `30` | 轮询模式下的检测间隔 |

### 工作流程

1. 启动 ZeroClaw daemon 时，如果 `[skills.hot_reload]` 配置启用，后台任务会自动开始监控 skills 目录
2. 编辑任何 skill 文件（如 `~/.zeroclaw/workspace/skills/my_skill/`）
3. 文件系统变化被检测到，经过防抖延迟后触发重载
4. 新的 skill tools 自动注册到所有活跃的 agent 会话
5. 日志输出显示重载状态和变更详情

### 日志示例

```
[INFO] Watching skills directory: "/home/user/.zeroclaw/workspace/skills" (debounce: 300ms)
[INFO] Skills directory changed, triggering reload
[INFO] Loaded 5 skills from directory
[INFO] Tool registry swapped to version 42
```

### 故障排查

**问题**: 热加载不工作
- 检查 `[skills.hot_reload.enabled]` 是否为 `true`
- 检查 skills 目录路径是否正确
- 尝试切换到 `watch_mode = "poll"` 模式

**问题**: 频繁重载
- 增加 `debounce_ms` 值（如 1000ms）
- 某些编辑器可能产生大量临时文件，考虑排除临时文件目录
