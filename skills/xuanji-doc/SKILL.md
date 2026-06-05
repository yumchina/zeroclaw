---
name: xuanji-doc-extraction
description: 处理文档（PPT/Word/Excel/PDF）的内容提取，通过璇玑Agent 异步提取文档中的文字和结构化信息
version: "0.2.1"
tags:
  - document
  - extraction
  - xuanji
enabled: true
---

# 璇玑文档提取

## 🚨 核心规则

当用户消息中包含 PPT、Word、Excel、PDF 文件时：

→ **第一步就直接调** `dawn_xuanji_create_task` 提交给璇玑Agent 处理，**不要先尝试任何其他工具**
→ **调用一次后立即停止，不要再调用第二次，无论是否收到结果通知**

收到 `[璇玑文档提取完成]` 或 `[璇玑文档提取失败]` 时：
→ 直接处理结果（读文件/告知失败），**不要再调用 dawn_xuanji_create_task**

## ❌ 绝对禁止

- ❌ **不要用 `file_read` 或其他本地文件读取工具直接读 PDF/PPT/Word/Excel 文件** — 这些工具对文档格式的提取能力不可靠，会返回乱码，浪费时间。文档提取必须走璇玑
- ❌ 不要用 `shell` 执行 pip install / bun add / python 等任何命令解析文档
- ❌ 不要自己写代码读取文档内容
- ❌ 不要用 `dawn_s3` 下载文件后自己处理
- ❌ 文件链接已在消息中，直接用

## ✅ 正确流程

1. **只调用一次** `dawn_xuanji_create_task`（`file_url` 用消息中 `<!-- file-url: ... -->` 注释里的 URL）
2. 告诉用户 "文件正在提取中，完成后主动通知您"
3. **等待 `extraction_complete` 通知，不要再重复调用 `dawn_xuanji_create_task`**
4. 用户询问进度时，调用 `dawn_xuanji_query_task`

支持：.pdf .docx .pptx .xlsx，多文件一次提交。
