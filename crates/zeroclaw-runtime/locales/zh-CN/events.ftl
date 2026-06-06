# 进度事件文案,供 channel 进度观察器使用。
event-agent-start = Agent 启动（{ $provider }/{ $model }）
event-agent-end = 处理完成
event-llm-request = 正在调用大模型推理（{ $count } 条消息）
event-tool-start-shell = 执行命令：{ $snippet }
event-tool-start-web-search = 搜索：{ $snippet }
event-tool-start-read-file = 读取文件：{ $snippet }
event-tool-start-http = HTTP 请求：{ $snippet }
event-tool-start-generic = 调用工具：{ $tool }
event-tool-done-success = { $tool } 执行完成（{ $elapsed }ms）
event-tool-done-failure = { $tool } 执行失败
event-error = { $component } 出现错误：{ $message }
