# WuKongIM 历史消息同步安全机制修复计划

> **对于智能体架构：** 推荐使用 superpowers:subagent-driven-development 或 superpowers:executing-plans 分步执行本计划。步骤使用复选框 (`- [ ]`) 语法进行进度跟踪。

**目标：** 修复 WuKongIM 历史消息同步逻辑，实现基于单个会话的“先处理后提交 (Process-then-Commit)”顺序水位线更新，并通过后台异步任务执行历史消息处理，防止在历史消息处理时间过长（如媒体文件下载慢）时阻塞通道启动或导致死锁。

**架构设计：**
1. **解除提前提交**：在 `sync_history` 的 HTTP 获取阶段，移除对 `self.update_sync_state` 的提前调用，改为返回拉取到的历史消息与最新的同步更新目标 `(Vec<RecvNotificationParams>, Vec<ConversationSyncUpdate>)`。
2. **异步后台处理**：在 `listen` 中，建立 WebSocket 连接并握手成功后，立即使用 `tokio::spawn` 启动后台任务处理历史消息。**主监听循环立即启动**，以便接收并响应心跳和实时消息，彻底避免启动阻塞和潜在的死锁。
3. **并发安全保护**：
   - 引入 `sync_state_lock: Arc<tokio::sync::Mutex<()>>` 序列化对同步状态文件 `wukongim_sync.json` 的读写，防止后台任务与实时监听任务并发写冲突。
   - 引入 `history_sync_complete: Arc<std::sync::atomic::AtomicBool>` 状态标识。在历史消息后台处理完成前，实时消息不写入磁盘位点（防止实时消息位点超前提交导致未处理完的历史消息丢失）。
4. **提前幂等过滤**：在后台任务中，将获取到的所有历史消息在分组前先进行幂等校验过滤（比对本地 DB 和文件中的已处理 sequence）。
5. **解除批量处理函数内的提交**：在 `process_offline_batch` 和 `send_offline_batch_as_single_message` 中移除持久化位点更新和 `clear_unread` 清理逻辑。
6. **后置安全水位线提交**：在所有离线批处理投递完成后，后置计算每个会话的安全提交 sequence（保证小于等于该 seq 的本批消息全部处理成功），并在此时安全地更新 `update_sync_state`、`clear_unread` 以及全局 `max_version`。最后将 `history_sync_complete` 设为 `true`。

**技术栈：** Rust 2024, WuKongIM, Tokio, anyhow.

---

## 需要用户评审

> [!WARNING]
> 本修复方案将改变 `zeroclaw-channel-wukongim` 的同步机制，将原本的“同步阻塞获取（Fetch-then-Commit）”升级为“后台异步处理与后置水位线提交（Process-then-Commit）”。该修复能显著提升启动稳定性和响应速度。

## 待讨论问题

无。

## 变更文件

### zeroclaw-channel-wukongim

#### [修改] [channel.rs](file:///Users/mengliang/project/zeroclaw/crates/zeroclaw-channel-wukongim/src/channel.rs)
- 在 `WuKongIMChannel` 结构体中新增 `sync_state_lock` 和 `history_sync_complete` 字段。
- 修改 `sync_history` 函数签名以返回 `Result<(Vec<RecvNotificationParams>, Vec<ConversationSyncUpdate>)>`。
- 去除 `sync_history` 循环内部的提前 `update_sync_state` 调用。
- 修改 `listen`：握手成功后，使用 `tokio::spawn` 异步处理历史消息，主任务直接进入实时 select 监听循环。
- 修改 `process_inbound_message` : 仅当 `history_sync_complete` 为 `true` 时，才调用 `update_sync_state` 保存已读位点。
- 在 `update_sync_state` 中加锁保证写操作的线程安全。
- 移除 `process_offline_batch` 和 `send_offline_batch_as_single_message` 中的状态保存逻辑。

## 验证计划

### 自动化测试
- 运行 `cargo test -p zeroclaw-channel-wukongim` 确保现有测试能通过。
- 编写新的单元测试 `test_history_sync_watermark_logic` 以验证在不同分组处理成功/失败情景下的水位线推进行为，以及后台异步处理时的状态同步。

### 手动验证
- 执行以下命令验证代码格式、Lint 和测试通过：
  ```bash
  cargo fmt --all -- --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  ```

---

## 计划任务

### 任务 1: 更新通道结构体定义与协议类型

**修改文件：**
- 修改：[channel.rs](file:///Users/mengliang/project/zeroclaw/crates/zeroclaw-channel-wukongim/src/channel.rs)

- [ ] **步骤 1：在 `WuKongIMChannel` 中添加并发安全字段**
  修改 `WuKongIMChannel` 结构体定义，引入：
  - `sync_state_lock: Arc<tokio::sync::Mutex<()>>`
  - `history_sync_complete: Arc<std::sync::atomic::AtomicBool>`

- [ ] **步骤 2：在 `from_config` 中初始化新增字段**
  在 `from_config` 初始化方法中实例化这两个字段。

- [ ] **步骤 3：定义 `ConversationSyncUpdate` 结构体**
  在 `channel.rs` 内部定义用于暂存会话最新状态的结构：
  ```rust
  #[derive(Debug, Clone)]
  struct ConversationSyncUpdate {
      channel_id: String,
      channel_type: u8,
      last_msg_seq: u32,
      version: i64,
  }
  ```

- [ ] **步骤 4：更新 `sync_history` 签名与逻辑**
  将 `sync_history` 返回类型更改为 `anyhow::Result<(Vec<RecvNotificationParams>, Vec<ConversationSyncUpdate>)>`。
  移除循环中的 `self.update_sync_state`，将需要更新的会话配置收集至 `updates` 向量中，并返回 `Ok((all_history, updates))`。

---

### 任务 2: 重构离线批量处理与消息发送逻辑

**修改文件：**
- 修改：[channel.rs](file:///Users/mengliang/project/zeroclaw/crates/zeroclaw-channel-wukongim/src/channel.rs)

- [ ] **步骤 1：移除 `send_offline_batch_as_single_message` 的持久化提交**
  删除 `send_offline_batch_as_single_message` 中向 `tx` 发送成功后调用的 `update_sync_state` 逻辑，仅在发送成功后返回 `Ok(())`。

- [ ] **步骤 2：重构 `process_offline_batch` 移除状态更新与未读清除**
  移除 `process_offline_batch` 内的 `update_sync_state` 和 `clear_unread` 调用。

---

### 任务 3: 在 `listen` 和 `update_sync_state` 中实现后台异步处理和加锁保护

**修改文件：**
- 修改：[channel.rs](file:///Users/mengliang/project/zeroclaw/crates/zeroclaw-channel-wukongim/src/channel.rs)

- [ ] **步骤 1：在 `update_sync_state` 中加入互斥锁**
  使用 `self.sync_state_lock.lock().await` 序列化状态文件更新操作，避免多任务写冲突。

- [ ] **步骤 2：修改 `process_inbound_message` 位点提交时机**
  仅当 `self.history_sync_complete` 为 `true` 时，才调用 `update_sync_state` 保存实时消息的已读位点。

- [ ] **步骤 3：在 `listen` 中通过 `tokio::spawn` 异步处理历史消息**
  在 `listen` 握手成功后，使用 `tokio::spawn` 开启后台任务：
  1. 比对当前 sequence 过滤掉已读消息。
  2. 对未读历史消息按 topic 分组投递并记录成败。
  3. 投递完毕后计算每个会话的安全水位线，调用 `update_sync_state` 和 `clear_unread`。
  4. 如果全局所有历史消息投递成功，更新全局 `max_version`。
  5. 将 `history_sync_complete` 设为 `true`。
  主任务在此期间无需等待，直接进入实时监听 select 循环。

- [ ] **步骤 4：运行测试与 Clippy 验证编译**
  运行：`cargo clippy --all-targets -- -D warnings`
  预期输出：成功无警告

---

### 任务 4: 添加单元测试

**修改文件：**
- 修改：[channel.rs](file:///Users/mengliang/project/zeroclaw/crates/zeroclaw-channel-wukongim/src/channel.rs)

- [ ] **步骤 1：编写水位线与异步处理单元测试**
  在 `tests` 模块底部添加 `test_history_sync_watermark_logic`，验证在部分 Topic 投递成功或全部失败等各种场景下，安全水位线位点更新的正确性，以及后台异步处理时的状态同步。

- [ ] **步骤 2：运行所有测试**
  运行：`cargo test -p zeroclaw-channel-wukongim`
  预期输出：所有测试全部通过
