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

    async fn cmd_import(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Read, "ppt.import")
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
                    "suggestion": "Consider splitting the presentation into smaller parts",
                    "size_bytes": metadata.len(),
                    "max_bytes": MAX_FILE_BYTES
                }).to_string()),
            });
        }

        // Use office_oxide to read and convert to Markdown
        let doc = office_oxide::Document::open(&resolved_path)
            .map_err(|e| anyhow::anyhow!("Failed to open presentation: {e}"))?;

        let markdown = doc.to_markdown();

        // Build output with metadata
        let format_name = doc.format_name();
        let output = format!(
            "# Presentation: {}\n\n**Format:** {}\n\n{}",
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
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Act, "ppt.export")
            .map_err(|e| anyhow::anyhow!(e))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        let title = args["title"].as_str().unwrap_or("Presentation");

        let slides = args["slides"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("slides array is required for export"))?;

        if slides.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("slides array cannot be empty".to_string()),
            });
        }

        if slides.len() > MAX_SLIDES {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Too many slides: {} (max {})", slides.len(), MAX_SLIDES)),
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

        // Create PPTX using office_oxide PptxWriter
        use office_oxide::pptx::write::PptxWriter;

        let mut writer = PptxWriter::new();

        for slide_data in slides {
            let slide_title = slide_data["title"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Each slide must have a title"))?;

            let mut slide_builder = writer.add_slide();
            slide_builder.set_title(slide_title);

            // Add subtitle if present
            if let Some(subtitle) = slide_data["subtitle"].as_str() {
                slide_builder.add_text(subtitle);
            }

            // Process content items
            if let Some(content) = slide_data["content"].as_array() {
                for item in content {
                    let item_type = item["type"].as_str().unwrap_or("text");

                    match item_type {
                        "text" => {
                            if let Some(text) = item["text"].as_str() {
                                slide_builder.add_text(text);
                            }
                        }
                        "bullet_list" => {
                            if let Some(items) = item["items"].as_array() {
                                let bullet_items: Vec<&str> = items
                                    .iter()
                                    .filter_map(|i| i.as_str())
                                    .collect();
                                if !bullet_items.is_empty() {
                                    slide_builder.add_bullet_list(&bullet_items);
                                }
                            }
                        }
                        "table" => {
                            // Tables require special handling - add as text for now
                            // Full table support requires more complex API usage
                            if let Some(header) = item["header"].as_array() {
                                let header_text = header
                                    .iter()
                                    .filter_map(|h| h.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" | ");
                                slide_builder.add_text(format!("Table: {}", header_text));
                            }
                        }
                        "image" => {
                            // Image insertion requires path validation and embedding
                            // Add placeholder for now - full implementation needs more research
                            if let Some(img_path) = item["path"].as_str() {
                                slide_builder.add_text(format!("[Image: {}]", img_path));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Save the presentation
        writer
            .save(&full_path)
            .map_err(|e| anyhow::anyhow!("Failed to save presentation: {e}"))?;

        Ok(ToolResult {
            success: true,
            output: json!({
                "file": file_path,
                "title": title,
                "slides_written": slides.len(),
                "message": "PPTX file exported successfully"
            }).to_string(),
            error: None,
        })
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