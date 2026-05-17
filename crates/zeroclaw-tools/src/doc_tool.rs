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