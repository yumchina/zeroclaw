---
name: xuanji-doc-extraction
description: 处理文档（PPT/Word/Excel/PDF）的内容提取，通过璇玑Agent 异步提取文档中的文字和结构化信息
version: "0.2.0"
tags:
  - document
  - extraction
  - xuanji
enabled: true
---

# 璇玑文档提取

## 🚨 核心规则

当用户消息中包含 PPT、Word、Excel、PDF 文件时，你**只有一个选择**：

→ 调用 `xuanji_doc_create_task` 提交给璇玑Agent 处理。

## ❌ 绝对禁止

- ❌ 不要用 `shell` 执行 pip install / bun add / python 等任何命令解析文档
- ❌ 不要自己写代码读取文档内容
- ❌ 不要用 `dawn_s3` 下载文件后自己处理
- ❌ 文件 URL 已在消息中，直接用

## ✅ 正确流程

1. 调用 `xuanji_doc_create_task`（`file_url` 用消息中已有的 S3 URL）
2. 告诉用户 "文件正在提取中，预计 30-60 秒，完成后主动通知您"
3. 用户询问进度时，调用 `xuanji_doc_query_task`

支持：.pdf .docx .pptx .xlsx，多文件一次提交。
