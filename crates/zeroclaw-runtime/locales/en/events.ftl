# Progress event strings, consumed by the channel progress observer.
event-agent-start = Agent started ({ $provider }/{ $model })
event-agent-end = Done
event-llm-request = Calling LLM ({ $count } messages)
event-tool-start-shell = Running command: { $snippet }
event-tool-start-web-search = Searching: { $snippet }
event-tool-start-read-file = Reading file: { $snippet }
event-tool-start-http = HTTP request: { $snippet }
event-tool-start-generic = Calling tool: { $tool }
event-tool-done-success = { $tool } completed ({ $elapsed }ms)
event-tool-done-failure = { $tool } failed
event-error = { $component } error: { $message }
