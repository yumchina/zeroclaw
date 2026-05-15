//! Excel import/export tool using calamine + rust_xlsxwriter.
//!
//! - calamine: read .xls/.xlsx/.csv/ods files
//! - rust_xlsxwriter: write formatted .xlsx reports

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum file size for Excel operations (50 MB).
const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
/// Maximum rows to export in a single operation.
const MAX_EXPORT_ROWS: usize = 100_000;

pub struct ExcelTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
}

impl ExcelTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }
}

#[async_trait]
impl Tool for ExcelTool {
    fn name(&self) -> &str {
        "excel"
    }

    fn description(&self) -> &str {
        "Excel import/export tool: read .xls/.xlsx/.csv/ods files with advanced features, \
         generate formatted .xlsx reports with charts and validation rules"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "required": ["command", "file_path"],
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["import", "export", "report", "validate", "info"],
                    "description": "Excel operation command"
                },
                "file_path": {
                    "type": "string",
                    "description": "Path to Excel file (relative to workspace)"
                },
                "sheet": {
                    "type": "string",
                    "description": "Sheet name (default: first sheet)"
                },
                "data": {
                    "type": "array",
                    "description": "Data array for export (array of arrays)"
                },
                "title": {
                    "type": "string",
                    "description": "Report title"
                },
                "sections": {
                    "type": "array",
                    "description": "Report sections with title and data"
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
            "report" => self.cmd_report(&args).await,
            "validate" => self.cmd_validate(&args).await,
            "info" => self.cmd_info(&args).await,
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown command: {other}")),
            }),
        }
    }
}

impl ExcelTool {
    async fn cmd_import(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Read, "excel.import")
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("import command not yet implemented".to_string()),
        })
    }

    async fn cmd_export(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Act, "excel.export")
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("export command not yet implemented".to_string()),
        })
    }

    async fn cmd_report(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Act, "excel.report")
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("report command not yet implemented".to_string()),
        })
    }

    async fn cmd_validate(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(
                zeroclaw_config::policy::ToolOperation::Read,
                "excel.validate",
            )
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("validate command not yet implemented".to_string()),
        })
    }

    async fn cmd_info(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Read, "excel.info")
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("info command not yet implemented".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn tool_name_is_excel() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());
        assert_eq!(tool.name(), "excel");
    }

    #[test]
    fn parameters_schema_has_command() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        let commands = schema["properties"]["command"]["enum"].as_array().unwrap();
        assert_eq!(commands.len(), 5);
        assert!(commands.contains(&json!("import")));
        assert!(commands.contains(&json!("export")));
        assert!(commands.contains(&json!("report")));
        assert!(commands.contains(&json!("validate")));
        assert!(commands.contains(&json!("info")));
    }
}
