//! PPT/PPTX import/export tool using office_oxide.
//!
//! - office_oxide: read .ppt/.pptx files, write .pptx files

use async_trait::async_trait;
use quick_xml::events::Event;
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use zip::ZipArchive;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum file size for PPT operations (50 MB).
const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
/// Maximum slides to process or export.
const MAX_SLIDES: usize = 200;
/// Maximum size for a single slide XML file (5 MB).
const MAX_SLIDE_XML_BYTES: usize = 5 * 1024 * 1024;
/// Maximum slides to process in direct ZIP extraction.
const MAX_DIRECT_SLIDES: usize = 200;

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

        // Detect format and use appropriate API
        let ext = resolved_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        let (format_name, slide_count, markdown) = if ext == "pptx" {
            // Workaround: office_oxide has a bug where it only reads 1 slide
            // from PPTX files with p14:sectionLst extensions.
            // We directly parse the ZIP file to extract text from all slides.
            extract_pptx_slides_directly(&resolved_path)?
        } else if ext == "ppt" {
            // Use PptDocument for legacy PPT files
            use office_oxide::ppt::PptDocument;
            let ppt = PptDocument::open(&resolved_path)
                .map_err(|e| anyhow::anyhow!("Failed to open PPT: {e}"))?;

            // PPT only supports plain_text extraction
            let text = ppt.plain_text();
            ("PPT", 0, text)
        } else {
            // Fallback to generic Document API
            let doc = office_oxide::Document::open(&resolved_path)
                .map_err(|e| anyhow::anyhow!("Failed to open presentation: {e}"))?;

            let md = doc.to_markdown();
            ("Unknown", 0, md)
        };

        // Build output with metadata
        let output = format!(
            "# Presentation: {}\n\n**Format:** {}\n\n**Slides:** {}\n\n{}",
            file_path,
            format_name,
            slide_count,
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

            let slide_builder = writer.add_slide();
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
                                slide_builder.add_text(&format!("Table: {}", header_text));
                            }
                        }
                        "image" => {
                            // Image insertion requires path validation and embedding
                            // Add placeholder for now - full implementation needs more research
                            if let Some(img_path) = item["path"].as_str() {
                                slide_builder.add_text(&format!("[Image: {}]", img_path));
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

/// Directly extract text from PPTX slides by parsing the ZIP file.
///
/// office_oxide's presentation.xml parser has a bug where it matches all
/// `<XXX:sldIdLst>` elements (including p14:sectionLst), overwriting the
/// slides list. This function directly parses the ZIP to extract text from
/// each slide XML file.
fn extract_pptx_slides_directly(path: &std::path::Path) -> anyhow::Result<(&'static str, usize, String)> {
    let file = std::fs::File::open(path)?;
    let mut archive = ZipArchive::new(file)?;

    // Find all slide files (ppt/slides/slide*.xml)
    let slide_files: Vec<(usize, String)> = archive
        .file_names()
        .filter(|name| {
            let normalized = name.replace('\\', "/");
            normalized.starts_with("ppt/slides/slide")
                && normalized.ends_with(".xml")
                && !normalized.contains("_rels")
        })
        .filter_map(|name| {
            // Extract slide number from filename
            let normalized = name.replace('\\', "/");
            let num = normalized
                .trim_start_matches("ppt/slides/slide")
                .trim_end_matches(".xml")
                .parse::<usize>()
                .ok()?;
            Some((num, normalized))
        })
        .collect();

    // Sort by slide number
    let mut sorted_slides = slide_files;
    sorted_slides.sort_by_key(|(num, _)| *num);

    // Limit number of slides to prevent memory exhaustion
    let total_slides = sorted_slides.len();
    if sorted_slides.len() > MAX_DIRECT_SLIDES {
        tracing::warn!(
            "PPTX has {} slides, truncating to {} for extraction",
            total_slides,
            MAX_DIRECT_SLIDES
        );
        sorted_slides.truncate(MAX_DIRECT_SLIDES);
    }

    // Extract text from each slide
    let mut parts = Vec::new();
    for (slide_num, zip_path) in sorted_slides {
        let index = archive.index_for_name(&zip_path).ok_or_else(|| {
            anyhow::anyhow!("Slide file not found: {}", zip_path)
        })?;

        let file = archive.by_index(index)?;

        // Check individual XML file size
        let file_size = file.size() as usize;
        if file_size > MAX_SLIDE_XML_BYTES {
            tracing::warn!(
                "Slide {} XML too large ({} bytes), skipping",
                slide_num,
                file_size
            );
            continue;
        }

        let mut content = Vec::new();
        // Use take() as extra protection against oversized files
        file.take(MAX_SLIDE_XML_BYTES as u64).read_to_end(&mut content)?;

        let text = extract_text_from_slide_xml(&content);
        if !text.is_empty() {
            parts.push(format!("## Slide {}\n\n{}", slide_num, text));
        }
    }

    Ok(("PPTX", total_slides, parts.join("\n\n---\n\n")))
}

/// Extract text content from a slide XML file.
///
/// Parses DrawingML text elements (<a:t>) to extract plain text.
fn extract_text_from_slide_xml(xml_data: &[u8]) -> String {
    let mut reader = quick_xml::Reader::from_reader(xml_data);
    reader.config_mut().trim_text(true);

    let mut texts = Vec::new();
    let mut in_text_elem = false;
    let mut current_paragraph: Vec<String> = Vec::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                // <a:t> contains actual text
                if e.local_name().as_ref() == b"t" {
                    in_text_elem = true;
                }
                // <a:p> is a paragraph - we'll add a newline when we exit
                if e.local_name().as_ref() == b"p" {
                    current_paragraph.clear();
                }
            }
            Ok(Event::End(ref e)) => {
                if e.local_name().as_ref() == b"t" {
                    in_text_elem = false;
                }
                if e.local_name().as_ref() == b"p" && !current_paragraph.is_empty() {
                    texts.push(current_paragraph.join(" "));
                    current_paragraph.clear();
                }
            }
            Ok(Event::Text(ref t)) if in_text_elem => {
                let text = t.decode().map(|s| s.to_string()).unwrap_or_default();
                if !text.is_empty() {
                    current_paragraph.push(text);
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }

    // Add any remaining paragraph
    if !current_paragraph.is_empty() {
        texts.push(current_paragraph.join(" "));
    }

    texts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use zeroclaw_config::policy::SecurityPolicy;

    fn create_test_tool() -> PptTool {
        let security = Arc::new(SecurityPolicy::default());
        let workspace = std::env::temp_dir();
        PptTool::new(security, workspace)
    }

    #[test]
    fn test_tool_name() {
        let tool = create_test_tool();
        assert_eq!(tool.name(), "ppt");
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
            "file_path": "test.pptx"
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
            "file_path": "test.pptx"
        });

        let result = tool.execute(args).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unknown command"));
    }
}