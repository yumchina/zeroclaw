# 设计文档：WuKongIM 离线消息批处理过滤器

为 WuKongIM 通道设计并实现一个精细的离线消息批处理过滤器，以避免在历史消息同步时将助手的系统/状态更新、卡片审批或交互消息并入用户的上下文，从而彻底解决引起 Orchestrator 做出不回复 (`NoReply`) 决策的故障。

## 1. 问题背景与根源

当 ZeroClaw Daemon 重启、网络掉线重连或重新初始化时，系统会触发 `sync_history()` 来拉取最近的离线历史消息。

### 1.1 故障机理
1. **全量拉取与合并**：WuKongIM 通道会将收到的历史消息按时间顺序放入一个 Batch，并在 `send_offline_batch_as_single_message` 中合并为一条 Combined Message。
2. **非对话消息污染**：此 Batch 中除了用户的文本外，还夹杂了助手上一轮运行时自动投递的日志级进度指示 (type 23)、发送的审批卡片 (type 20)、卡片的确认与反馈指令 (type 21 / type 99) 等非用户会话消息。
3. **首尾颠倒与意图误判**：由于这些被包含在 Batch 中的系统级消息是助手自己发出的，最终组装出的大消息在分发至 Orchestrator 时，其末尾部分的发送者会被识别为助手。
4. **决策陷入沉默**：在 Orchestrator 的 `classify_channel_reply_intent` 预检中，LLM 会将这一表现识别为“助手本身是最后发言的人，无需回复”，因而判定为 `NoReply`，造成了离线历史消息加载后助手沉默不语的 Bug。

## 2. 解决方案设计

为了排除非对话类历史消息对意图分类的负面干扰，我们对离线批消息流程进行深度重构，在将其提交给大模型前执行过滤净化。

### 2.1 过滤器机制

在合并历史批消息前，应剔除所有属于助手自身（`bot`）的消息或不属于普通文本对话的交互事件消息。

对于每一条历史消息 `m`，若满足以下任何条件，则直接将其从待回复的批消息中过滤掉：
1. **发送者为助手**：`m.from_uid == self.uid`（自身发送的历史响应、状态或卡片）。
2. **指令类消息 (CMD)**：`type == 99` 或 `payload.cmd` 不为空。
3. **互动卡片消息 (InteractiveCard)**：`type == 20`。
4. **互动卡片响应消息 (InteractiveResponse)**：`type == 21`。
5. **进度状态指示消息 (StatusUpdate)**：`type == 23`。

### 2.2 同步状态推进保障

由于部分批次消息可能在过滤后变为空（例如该批次全是助手此前单向输出的进度更新），因此必须对空批次和非空批次进行分流处理：

*   **分流一：过滤后消息列表为空**：
    *   此时无任何有效用户消息需要回复，绝对不能生成 combined message 投递给 Orchestrator（避免触发无意义的 LLM 空运行）。
    *   直接原地推进同步状态：调用 `self.update_sync_state`，将序列号 (last_seq) 和时间戳 (timestamp) 原子对齐到当前批次的最新位点。
    *   调用 `self.clear_unread` 清理服务器上的未读计数。
*   **分流二：过滤后消息列表非空**：
    *   仅将筛选出的纯净用户对话消息传递给 `send_offline_batch_as_single_message` 进行拼接。
    *   拼接的 `ChannelMessage` 的 `sender` 明确设置为用户的 `target_id`（个人聊天中为发送方的 `from_uid`，群聊中为 `channel_id`），保证预检判定它是来自用户的有效请求。
    *   在消息成功发送后，原子更新同步状态位点并清理未读。

## 3. 技术实施路径

1.  **添加过滤机制**：在 `crates/zeroclaw-channel-wukongim/src/channel.rs` 的 `process_offline_batch` 中，使用 Rust 迭代器的 `.filter()` 构造纯净的 `filtered_messages` 列表。
2.  **实现空批次分流**：新增 `filtered_messages.is_empty()` 判断逻辑，静默更新同步位点，跳过 combined 拼接与分发。
3.  **实现发送人身份校正**：确保将 `filtered_messages`、最新的 `last_seq` 和 `last.timestamp` 正确传导并投递至下游，完成最终的 `update_sync_state` 与 `clear_unread` 操作。
