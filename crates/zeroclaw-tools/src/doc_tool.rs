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