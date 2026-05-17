//! PPT/PPTX import/export tool using office_oxide.
//!
//! - office_oxide: read .ppt/.pptx files, write .pptx files

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum file size for PPT operations (50 MB).
const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
/// Maximum slides to process or export.
const MAX_SLIDES: usize = 200;

pub struct PptTool {
    security: Arc<SecurityPolicy>,
    #[expect(dead_code)] // Reserved for future relative path resolution
    workspace_dir: PathBuf,
}

impl PptTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }
}

#[async_trait]
impl Tool for PptTool {
    fn name(&self) -> &str {
        "ppt"
    }

    fn description(&self) -> &str {
        "PPT/PPTX import/export tool: read .ppt/.pptx files and extract content as Markdown, \
         create new .pptx presentations with slides, text, bullet lists, images, and tables"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["command", "file_path"],
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["import", "export"],
                    "description": "PPT operation command"
                },
                "file_path": {
                    "type": "string",
                    "description": "Path to PPT file (relative to workspace)"
                },
                "title": {
                    "type": "string",
                    "description": "Presentation title for export"
                },
                "slides": {
                    "type": "array",
                    "description": "Slides array for export",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": {
                                "type": "string",
                                "description": "Slide title"
                            },
                            "subtitle": {
                                "type": "string",
                                "description": "Slide subtitle"
                            },
                            "content": {
                                "type": "array",
                                "description": "Slide content items",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "type": {
                                            "type": "string",
                                            "enum": ["text", "bullet_list", "image", "table"]
                                        },
                                        "text": {
                                            "type": "string",
                                            "description": "Text content for 'text' type"
                                        },
                                        "items": {
                                            "type": "array",
                                            "items": {"type": "string"},
                                            "description": "Bullet items for 'bullet_list' type"
                                        },
                                        "path": {
                                            "type": "string",
                                            "description": "Image path for 'image' type"
                                        },
                                        "header": {
                                            "type": "array",
                                            "items": {"type": "string"},
                                            "description": "Table header for 'table' type"
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
                        },
                        "required": ["title"]
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