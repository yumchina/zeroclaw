---
name: xuanji-doc-extraction
description: 处理文档（PPT/Word/Excel/PDF）的内容提取，通过璇玑Agent 异步提取文档中的文字和结构化信息
version: "0.1.0"
tags:
  - document
  - extraction
  - xuanji
enabled: true
tools:
  - name: xuanji_doc_create_task
    description: >
      向璇玑Agent 提交文档内容提取任务。传入文件的 S3 URL、文件名、文件类型和用户原文，
      返回 execution_id。支持 pdf/docx/pptx/xlsx 格式。支持一次提交多个文件。
    kind: script
    command: "{SKILL_DIR}/scripts/create_task.py"
    args:
      user_text: 用户发送文件时的原始文字消息
      files: 文件列表 JSON 数组 [{"file_url": "...", "file_name": "...", "file_type": "docx"}]
      la_id: 当前 yumclaw 实例的 la_id
    timeout_secs: 15
  - name: xuanji_doc_query_task
    description: >
      查询璇玑Agent 文档提取任务的进度和结果。
      传入 execution_id，返回当前状态（pending/running/completed/failed）和结果内容。
    kind: script
    command: "{SKILL_DIR}/scripts/query_task.py"
    args:
      execution_id: 任务 ID
    timeout_secs: 10
---

# 璇玑文档提取

当用户上传或发送 PPT、Word、Excel、PDF 文件时：
1. 使用 `xuanji_doc_create_task` 提交文档内容提取任务
2. 提交后告诉用户 "文件正在提取中，预计30-60秒完成"
3. 用户主动询问进度时，使用 `xuanji_doc_query_task` 查询任务状态
4. **不要尝试自己读取或解析文件内容** — 所有文档类型都通过璇玑Agent 异步处理

## 支持的文件类型
.pdf .docx .pptx .xlsx

## 多文件处理
当用户一次发送多个文档时，将所有文件打包在一次 `xuanji_doc_create_task` 调用中：
- `--files` 参数传入 JSON 数组，每个元素包含 file_url/file_name/file_type
- `--user_text` 传入用户的原始文字消息
- 文件顺序与用户上传顺序一致

## 任务参数

**创建任务** (`xuanji_doc_create_task`)：
- `user_text`：用户发送文件时的原始文字
- `files`：JSON 数组 [{"file_url": "...", "file_name": "...", "file_type": "docx"}]
- `la_id`：环境变量 `$LA_ID`

**查询任务** (`xuanji_doc_query_task`)：
- `execution_id`：创建任务时返回的 execution_id
