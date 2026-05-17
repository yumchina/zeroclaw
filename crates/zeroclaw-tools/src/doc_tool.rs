//! DOC/DOCX import/export tool using office_oxide.
//!
//! - office_oxide: read .doc/.docx files, write .docx files

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum file size for DOC operations (50 MB).
const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
/// Maximum content items to process or export.
const MAX_CONTENT_ITEMS: usize = 1000;

pub struct DocTool {
    security: Arc<SecurityPolicy>,
    #[expect(dead_code)] // Reserved for future relative path resolution
    workspace_dir: PathBuf,
}

impl DocTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }

    async fn cmd_import(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Read, "doc.import")
            .map_err(|e| anyhow::anyhow!(e))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        // Resolve and validate file path
        let full_path = self.security.resolve_tool_path(file_path);

        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(json!({
                        "error": format!("File not found: {}", file_path),
                        "suggestion": "Check the file path is correct and the file exists in workspace",
                        "path": file_path
                    }).to_string()),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.security.resolved_path_violation_message(&resolved_path)),
            });
        }

        // Check file size
        let metadata = tokio::fs::metadata(&resolved_path).await?;
        if metadata.len() as usize > MAX_FILE_BYTES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(json!({
                    "error": format!("File too large: {} bytes (max {})", metadata.len(), MAX_FILE_BYTES),
                    "suggestion": "Consider splitting the document into smaller parts",
                    "size_bytes": metadata.len(),
                    "max_bytes": MAX_FILE_BYTES
                }).to_string()),
            });
        }

        // Use office_oxide to read and convert to Markdown
        let doc = office_oxide::Document::open(&resolved_path)
            .map_err(|e| anyhow::anyhow!("Failed to open document: {e}"))?;

        let markdown = doc.to_markdown();

        // Build output with metadata
        let format_name = doc.format();
        let output = format!(
            "# Document: {}\n\n**Format:** {:?}\n\n{}",
            file_path,
            format_name,
            markdown
        );

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }

    async fn cmd_export(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Act, "doc.export")
            .map_err(|e| anyhow::anyhow!(e))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        let title = args["title"].as_str().unwrap_or("Document");

        let content = args["content"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("content array is required for export"))?;

        if content.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("content array cannot be empty".to_string()),
            });
        }

        if content.len() > MAX_CONTENT_ITEMS {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Too many content items: {} (max {})", content.len(), MAX_CONTENT_ITEMS)),
            });
        }

        // Resolve and validate output path
        let full_path = self.security.resolve_tool_path(file_path);
        if !self.security.is_resolved_path_allowed(&full_path) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.security.resolved_path_violation_message(&full_path)),
            });
        }

        // Check rate limit
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".to_string()),
            });
        }

        // Create DOCX using office_oxide DocxWriter
        use office_oxide::docx::write::DocxWriter;

        let mut writer = DocxWriter::new();

        for item in content {
            let item_type = item["type"].as_str().unwrap_or("paragraph");

            match item_type {
                "heading" => {
                    let level = item["level"].as_u64().unwrap_or(1) as u8;
                    let text = item["text"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("heading requires 'text' field"))?;
                    writer.add_heading(text, level);
                }
                "paragraph" => {
                    let text = item["text"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("paragraph requires 'text' field"))?;
                    writer.add_paragraph(text);
                }
                "list" => {
                    let items = item["items"]
                        .as_array()
                        .ok_or_else(|| anyhow::anyhow!("list requires 'items' array"))?;
                    let list_items: Vec<&str> = items
                        .iter()
                        .filter_map(|i| i.as_str())
                        .collect();
                    let ordered = item["ordered"].as_bool().unwrap_or(false);
                    if !list_items.is_empty() {
                        writer.add_list(&list_items, ordered);
                    }
                }
                "table" => {
                    let header = item["header"]
                        .as_array()
                        .ok_or_else(|| anyhow::anyhow!("table requires 'header' array"))?;
                    let data = item["data"]
                        .as_array()
                        .ok_or_else(|| anyhow::anyhow!("table requires 'data' array"))?;

                    // Build table rows
                    let header_row: Vec<String> = header
                        .iter()
                        .filter_map(|h| h.as_str().map(|s| s.to_string()))
                        .collect();

                    let mut table_data: Vec<Vec<String>> = vec![header_row];
                    for row in data {
                        let row_data: Vec<String> = row
                            .as_array()
                            .unwrap_or(&vec![])
                            .iter()
                            .map(|cell| {
                                match cell {
                                    serde_json::Value::String(s) => s.clone(),
                                    serde_json::Value::Number(n) => n.to_string(),
                                    serde_json::Value::Bool(b) => b.to_string(),
                                    _ => String::new(),
                                }
                            })
                            .collect();
                        table_data.push(row_data);
                    }

                    // Convert to string vectors for add_table
                    let table_rows: Vec<Vec<&str>> = table_data
                        .iter()
                        .map(|row| row.iter().map(|s| s.as_str()).collect())
                        .collect();

                    writer.add_table(&table_rows);
                }
                _ => {}
            }
        }

        // Save the document
        writer
            .save(&full_path)
            .map_err(|e| anyhow::anyhow!("Failed to save document: {e}"))?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "file": file_path,
                "title": title,
                "items_written": content.len(),
                "message": "DOCX file exported successfully"
            }).to_string(),
            error: None,
        })
    }
}

#[async_trait]
impl Tool for DocTool {
    fn name(&self) -> &str {
        "doc"
    }

    fn description(&self) -> &str {
        "DOC/DOCX import/export tool: read .doc/.docx files and extract content as Markdown, \
         create new .docx documents with headings, paragraphs, lists, and tables"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["command", "file_path"],
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["import", "export"],
                    "description": "DOC operation command"
                },
                "file_path": {
                    "type": "string",
                    "description": "Path to DOC file (relative to workspace)"
                },
                "title": {
                    "type": "string",
                    "description": "Document title for export"
                },
                "content": {
                    "type": "array",
                    "description": "Document content items for export",
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": {
                                "type": "string",
                                "enum": ["heading", "paragraph", "list", "table"],
                                "description": "Content item type"
                            },
                            "level": {
                                "type": "integer",
                                "minimum": 1,
                                "maximum": 6,
                                "description": "Heading level (1-6) for 'heading' type"
                            },
                            "text": {
                                "type": "string",
                                "description": "Text content for 'heading' or 'paragraph' type"
                            },
                            "items": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "List items for 'list' type"
                            },
                            "ordered": {
                                "type": "boolean",
                                "description": "Whether list is ordered (default: false)"
                            },
                            "header": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Table header row for 'table' type"
                            },
                            "data": {
                                "type": "array",
                                "items": {
                                    "type": "array",
                                    "items": {
                                        "oneOf": [
                                            {"type": "string"},
                                            {"type": "number"},
                                            {"type": "boolean"},
                                            {"type": "null"}
                                        ]
                                    }
                                },
                                "description": "Table data rows for 'table' type"
                            }
                        },
                        "required": ["type"]
                    }
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = match args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("'command' parameter is required".to_string()),
                });
            }
        };

        match command {
            "import" => self.cmd_import(&args).await,
            "export" => self.cmd_export(&args).await,
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown command: {other}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeroclaw_config::policy::SecurityPolicy;

    fn create_test_tool() -> DocTool {
        let security = Arc::new(SecurityPolicy::default());
        let workspace = std::env::temp_dir();
        DocTool::new(security, workspace)
    }

    #[test]
    fn test_tool_name() {
        let tool = create_test_tool();
        assert_eq!(tool.name(), "doc");
    }

    #[test]
    fn test_tool_description() {
        let tool = create_test_tool();
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn test_parameters_schema_has_required_fields() {
        let tool = create_test_tool();
        let schema = tool.parameters_schema();

        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("command")));
        assert!(required.contains(&serde_json::json!("file_path")));

        let commands = schema["properties"]["command"]["enum"].as_array().unwrap();
        assert!(commands.contains(&serde_json::json!("import")));
        assert!(commands.contains(&serde_json::json!("export")));
    }

    #[tokio::test]
    async fn test_missing_command_returns_error() {
        let tool = create_test_tool();
        let args = json!({
            "file_path": "test.docx"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("'command' parameter is required"));
    }

    #[tokio::test]
    async fn test_unknown_command_returns_error() {
        let tool = create_test_tool();
        let args = json!({
            "command": "invalid",
            "file_path": "test.docx"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown command"));
    }
}