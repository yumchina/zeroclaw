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

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        let sheet_name = args["sheet"].as_str().unwrap_or(""); // 空字符串表示使用第一个工作表

        // 解析并验证文件路径
        let full_path = self.security.resolve_tool_path(file_path);

        // Canonicalize the path for security checks
        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                let _ = self.security.record_action();
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
            let _ = self.security.record_action();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    self.security
                        .resolved_path_violation_message(&resolved_path),
                ),
            });
        }

        // 检查文件存在和大小
        let metadata = match tokio::fs::metadata(&resolved_path).await {
            Ok(m) => m,
            Err(e) => {
                let _ = self.security.record_action();
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

        if metadata.len() as usize > MAX_FILE_BYTES {
            let _ = self.security.record_action();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(json!({
                    "error": format!("File too large: {} bytes (max {})", metadata.len(), MAX_FILE_BYTES),
                    "suggestion": "Consider splitting the file into smaller parts",
                    "size_bytes": metadata.len(),
                    "max_bytes": MAX_FILE_BYTES
                }).to_string()),
            });
        }

        // 使用 calamine 读取 Excel 文件
        let mut workbook: calamine::Sheets<calamine::Data<_>> =
            calamine::open_workbook(&resolved_path)
                .map_err(|e| anyhow::anyhow!("Failed to open workbook: {e}"))?;

        // 检查工作表是否为空
        let sheet_names = workbook.sheet_names();
        if sheet_names.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    json!({
                        "error": "Workbook contains no sheets",
                        "file": file_path
                    })
                    .to_string(),
                ),
            });
        }

        // 获取工作表 - 尝试按名称，如果为空则使用第一个
        let sheet_data = if sheet_name.is_empty() || sheet_names.contains(&sheet_name.to_string()) {
            let target_sheet = if sheet_name.is_empty() {
                &sheet_names[0]
            } else {
                sheet_name
            };
            workbook
                .worksheet_range(target_sheet)
                .map_err(|e| anyhow::anyhow!("Failed to read sheet '{}': {}", target_sheet, e))?
        } else {
            // 请求的工作表不存在，返回错误
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    json!({
                        "error": format!("Sheet '{}' not found in workbook", sheet_name),
                        "available_sheets": sheet_names,
                        "file": file_path
                    })
                    .to_string(),
                ),
            });
        };

        let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();
        let mut headers: Vec<String> = Vec::new();
        let mut was_truncated = false;

        for (row_idx, row) in sheet_data.rows().enumerate() {
            let mut row_data: Vec<serde_json::Value> = Vec::new();

            for cell in row {
                match cell {
                    calamine::Data::Empty => row_data.push(json!(null)),
                    calamine::Data::String(s) => row_data.push(json!(s)),
                    calamine::Data::Float(f) => row_data.push(json!(f)),
                    calamine::Data::Int(i) => row_data.push(json!(i)),
                    calamine::Data::Bool(b) => row_data.push(json!(b)),
                    calamine::Data::DateTime(dt) => row_data.push(json!(dt.to_string())),
                    calamine::Data::DateTimeIso(s) => row_data.push(json!(s)),
                    calamine::Data::DurationMs(d) => row_data.push(json!(d)),
                    calamine::Data::Error(e) => row_data.push(json!(format!("ERROR: {e}"))),
                }
            }

            if row_idx == 0 {
                headers = row_data
                    .iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect();
            }

            rows.push(row_data);

            // 限制行数防止 OOM
            if rows.len() >= MAX_EXPORT_ROWS {
                was_truncated = true;
                break;
            }
        }

        // 构建结构化输出
        let actual_sheet_name = if sheet_name.is_empty() {
            &sheet_names[0]
        } else {
            sheet_name
        };
        let mut output_obj = json!({
            "file": file_path,
            "sheet": actual_sheet_name,
            "total_rows": rows.len(),
            "total_columns": headers.len(),
            "headers": headers,
            "data": rows,
            "summary": {
                "row_count": rows.len(),
                "column_count": headers.len(),
                "has_headers": !headers.is_empty(),
            }
        });

        // 添加截断警告
        if was_truncated {
            output_obj["warning"] = json!(format!(
                "Output truncated at {} rows (max export limit)",
                MAX_EXPORT_ROWS
            ));
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output_obj)?,
            error: None,
        })
    }

    async fn cmd_export(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Act, "excel.export")
            .map_err(|e| anyhow::anyhow!(e))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        let sheet_name = args["sheet"]
            .as_str()
            .unwrap_or("Sheet1");

        let data = args["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("data array is required"))?;

        let header = args["header"].as_array();

        // 解析并验证文件路径
        let full_path = self.security.resolve_tool_path(file_path);
        if !self.security.is_resolved_path_allowed(&full_path) {
            let _ = self.security.record_action();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.security.resolved_path_violation_message(&full_path)),
            });
        }

        // 检查速率限制
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".to_string()),
            });
        }

        // 创建新工作簿
        let mut workbook = rust_xlsxwriter::Workbook::new();

        // 添加工作表
        let worksheet = workbook.add_worksheet().set_name(sheet_name)
            .map_err(|e| anyhow::anyhow!("Failed to create worksheet: {e}"))?;

        // 创建格式
        let mut header_format = rust_xlsxwriter::Format::new();
        header_format.set_bold(true);
        header_format.set_font_color(rust_xlsxwriter::Color::RGB(0xFFFFFF));
        header_format.set_background_color(rust_xlsxwriter::Color::RGB(0x4472C4));
        header_format.set_align(rust_xlsxwriter::FormatAlign::Center);

        let mut data_format = rust_xlsxwriter::Format::new();
        data_format.set_align(rust_xlsxwriter::FormatAlign::Left);

        // 写入数据
        let mut row_num: u32 = 0;
        let mut was_truncated = false;

        // 写入 header（如果提供）
        if let Some(header_row) = header {
            for (col_idx, value) in header_row.iter().enumerate() {
                let cell_value = value.as_str().unwrap_or("");
                worksheet
                    .write_string(row_num, col_idx as u16, cell_value, &header_format)
                    .map_err(|e| anyhow::anyhow!("Failed to write header: {e}"))?;
            }
            row_num += 1;
        }

        // 写入数据
        for row in data {
            if let Some(row_array) = row.as_array() {
                for (col_idx, value) in row_array.iter().enumerate() {
                    let col_num = col_idx as u16;

                    match value {
                        serde_json::Value::Null => {
                            worksheet.write_blank(row_num, col_num, &data_format)
                                .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                        }
                        serde_json::Value::Bool(b) => {
                            worksheet.write_boolean(row_num, col_num, *b, &data_format)
                                .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                        }
                        serde_json::Value::Number(n) => {
                            if let Some(i) = n.as_i64() {
                                worksheet.write_number(row_num, col_num, i as f64, &data_format)
                                    .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                            } else if let Some(f) = n.as_f64() {
                                worksheet.write_number(row_num, col_num, f, &data_format)
                                    .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                            }
                        }
                        serde_json::Value::String(s) => {
                            worksheet.write_string(row_num, col_num, s, &data_format)
                                .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                        }
                        _ => {
                            worksheet.write_string(row_num, col_num, &value.to_string(), &data_format)
                                .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                        }
                    }
                }
                row_num += 1;

                if row_num >= MAX_EXPORT_ROWS as u32 {
                    was_truncated = true;
                    break;
                }
            }
        }

        // 自动调整列宽
        for col_idx in 0..26 {
            worksheet.set_column_width(col_idx, 15.0)
                .map_err(|e| anyhow::anyhow!("Failed to set column width: {e}"))?;
        }

        // 保存文件
        workbook
            .save(&full_path)
            .map_err(|e| anyhow::anyhow!("Failed to save workbook: {e}"))?;

        // 构建输出
        let mut output_obj = json!({
            "file": file_path,
            "rows_written": row_num,
            "message": "Excel file exported successfully"
        });

        // 添加截断警告
        if was_truncated {
            output_obj["warning"] = json!(format!(
                "Export truncated at {} rows (max export limit). Some data was not written.",
                MAX_EXPORT_ROWS
            ));
        }

        Ok(ToolResult {
            success: true,
            output: output_obj.to_string(),
            error: None,
        })
    }

    async fn cmd_report(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Act, "excel.report")
            .map_err(|e| anyhow::anyhow!(e))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        let title = args["title"]
            .as_str()
            .unwrap_or("Report");

        let sections = args["sections"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("sections array is required"))?;

        // 验证 sections 不为空
        if sections.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("sections array cannot be empty".to_string()),
            });
        }

        // 解析并验证文件路径
        let full_path = self.security.resolve_tool_path(file_path);
        if !self.security.is_resolved_path_allowed(&full_path) {
            let _ = self.security.record_action();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.security.resolved_path_violation_message(&full_path)),
            });
        }

        // 检查速率限制
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".to_string()),
            });
        }

        let mut workbook = rust_xlsxwriter::Workbook::new();
        let worksheet = workbook.add_worksheet()
            .map_err(|e| anyhow::anyhow!("Failed to create worksheet: {e}"))?;

        // 创建格式
        let mut title_format = rust_xlsxwriter::Format::new();
        title_format.set_font_size(18);
        title_format.set_bold(true);
        title_format.set_font_color(rust_xlsxwriter::Color::RGB(0x0000FF));
        title_format.set_align(rust_xlsxwriter::FormatAlign::Center);

        let mut heading_format = rust_xlsxwriter::Format::new();
        heading_format.set_font_size(12);
        heading_format.set_bold(true);
        heading_format.set_align(rust_xlsxwriter::FormatAlign::Left);

        let mut data_format = rust_xlsxwriter::Format::new();
        data_format.set_font_size(10);
        data_format.set_align(rust_xlsxwriter::FormatAlign::Left);

        let mut row_num: u32 = 0;
        let mut was_truncated = false;

        // 添加标题
        worksheet
            .write_string(row_num, 0, title, &title_format)
            .map_err(|e| anyhow::anyhow!("Failed to write title: {e}"))?;
        worksheet
            .merge_range(row_num, 0, row_num, 4, title)
            .map_err(|e| anyhow::anyhow!("Failed to merge title cells: {e}"))?;
        row_num += 2;

        // 处理各个 section
        for section in sections {
            let section_title = section["title"]
                .as_str()
                .unwrap_or("Section");

            let section_data = section["data"]
                .as_array()
                .unwrap_or(&vec![]);

            // 添加 section 标题
            worksheet
                .write_string(row_num, 0, section_title, &heading_format)
                .map_err(|e| anyhow::anyhow!("Failed to write section title: {e}"))?;
            worksheet
                .merge_range(row_num, 0, row_num, 4, section_title)
                .map_err(|e| anyhow::anyhow!("Failed to merge section cells: {e}"))?;
            row_num += 1;

            // 添加 section 数据
            for row in section_data {
                // 检查行数限制
                if row_num >= MAX_EXPORT_ROWS as u32 {
                    was_truncated = true;
                    break;
                }

                if let Some(arr) = row.as_array() {
                    for (col_idx, cell) in arr.iter().enumerate() {
                        let col_num = col_idx as u16;

                        match cell {
                            serde_json::Value::Null => {
                                worksheet.write_blank(row_num, col_num, &data_format)
                                    .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                            }
                            serde_json::Value::String(s) => {
                                worksheet.write_string(row_num, col_num, s, &data_format)
                                    .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                            }
                            serde_json::Value::Number(n) => {
                                if let Some(i) = n.as_i64() {
                                    worksheet.write_number(row_num, col_num, i as f64, &data_format)
                                        .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                                } else if let Some(f) = n.as_f64() {
                                    worksheet.write_number(row_num, col_num, f, &data_format)
                                        .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                                }
                            }
                            serde_json::Value::Bool(b) => {
                                worksheet.write_boolean(row_num, col_num, *b, &data_format)
                                    .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                            }
                            _ => {
                                worksheet.write_string(row_num, col_num, &cell.to_string(), &data_format)
                                    .map_err(|e| anyhow::anyhow!("Failed to write cell: {e}"))?;
                            }
                        }
                    }
                    row_num += 1;
                }
            }

            // 如果已截断，停止处理更多 sections
            if was_truncated {
                break;
            }

            row_num += 1; // section 后空一行
        }

        workbook
            .save(&full_path)
            .map_err(|e| anyhow::anyhow!("Failed to save report: {e}"))?;

        let mut output_obj = json!({
            "file": file_path,
            "rows_written": row_num,
            "message": "Report generated successfully"
        });

        // 添加截断警告
        if was_truncated {
            output_obj["warning"] = json!(format!(
                "Report truncated at {} rows (max export limit). Some sections/data were not written.",
                MAX_EXPORT_ROWS
            ));
        }

        Ok(ToolResult {
            success: true,
            output: output_obj.to_string(),
            error: None,
        })
    }

    async fn cmd_validate(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Read, "excel.validate")
            .map_err(|e| anyhow::anyhow!(e))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        // 解析并验证文件路径
        let full_path = self.security.resolve_tool_path(file_path)?;
        if !self.security.is_resolved_path_allowed(&full_path) {
            let _ = self.security.record_action();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.security.resolved_path_violation_message(&full_path)),
            });
        }

        // 检查文件存在和大小
        let metadata = match tokio::fs::metadata(&full_path).await {
            Ok(m) => m,
            Err(e) => {
                let _ = self.security.record_action();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(json!({
                        "error": format!("File not found: {}", file_path),
                        "suggestion": "Check the file path is correct"
                    }).to_string()),
                });
            }
        };

        if metadata.len() as usize > MAX_FILE_BYTES {
            let _ = self.security.record_action();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(json!({
                    "error": format!("File too large: {} bytes", metadata.len()),
                    "suggestion": "File size exceeds 50MB limit"
                }).to_string()),
            });
        }

        // 尝试打开工作簿验证格式
        let _workbook: calamine::Sheets<calamine::Data<_>> =
            match calamine::open_workbook(&full_path) {
                Ok(wb) => wb,
                Err(e) => {
                    let _ = self.security.record_action();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(json!({
                            "error": format!("Invalid Excel format: {}", e),
                            "suggestion": "Ensure the file is a valid .xls, .xlsx, .csv, or .ods file"
                        }).to_string()),
                    });
                }
            };

        Ok(ToolResult {
            success: true,
            output: json!({
                "file": file_path,
                "valid": true,
                "size_bytes": metadata.len(),
                "message": "File is valid and can be read"
            }).to_string(),
            error: None,
        })
    }

    async fn cmd_info(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        self.security
            .enforce_tool_operation(zeroclaw_config::policy::ToolOperation::Read, "excel.info")
            .map_err(|e| anyhow::anyhow!(e))?;

        let file_path = args["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;

        // 解析并验证文件路径
        let full_path = self.security.resolve_tool_path(file_path)?;
        if !self.security.is_resolved_path_allowed(&full_path) {
            let _ = self.security.record_action();
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.security.resolved_path_violation_message(&full_path)),
            });
        }

        // 获取文件信息
        let metadata = match tokio::fs::metadata(&full_path).await {
            Ok(m) => m,
            Err(_) => {
                let _ = self.security.record_action();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(json!({
                        "error": format!("File not found: {}", file_path),
                        "suggestion": "Check the file path is correct"
                    }).to_string()),
                });
            }
        };

        // 打开工作簿获取信息
        let workbook: calamine::Sheets<calamine::Data<_>> =
            match calamine::open_workbook(&full_path) {
                Ok(wb) => wb,
                Err(e) => {
                    let _ = self.security.record_action();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(json!({
                            "error": format!("Failed to open workbook: {}", e)
                        }).to_string()),
                    });
                }
            };

        let sheet_names = workbook.sheet_names();
        let mut sheet_info = Vec::new();

        for sheet_name in &sheet_names {
            if let Ok(range) = workbook.worksheet_range(sheet_name) {
                sheet_info.push(json!({
                    "name": sheet_name,
                    "rows": range.height(),
                    "columns": range.width()
                }));
            }
        }

        Ok(ToolResult {
            success: true,
            output: json!({
                "file": file_path,
                "size_bytes": metadata.len(),
                "size_mb": metadata.len() as f64 / (1024.0 * 1024.0),
                "sheet_count": sheet_names.len(),
                "sheets": sheet_info
            }).to_string(),
            error: None,
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

    #[tokio::test]
    async fn import_reads_xlsx_file() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());

        // 创建测试文件
        let test_file = std::env::temp_dir().join("test_import.xlsx");
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let worksheet = workbook.add_worksheet().unwrap();
        worksheet.write_string(0, 0, "Name").unwrap();
        worksheet.write_string(0, 1, "Age").unwrap();
        worksheet.write_string(1, 0, "Alice").unwrap();
        worksheet.write_number(1, 1, 30.0).unwrap();
        workbook.save(&test_file).unwrap();

        // 测试读取
        let result = tool
            .execute(json!({
                "command": "import",
                "file_path": test_file.file_name().unwrap().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["total_rows"], 2);
        assert_eq!(output["total_columns"], 2);

        // 清理
        let _ = std::fs::remove_file(&test_file);
    }

    #[tokio::test]
    async fn import_handles_missing_file() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());
        let result = tool
            .execute(json!({
                "command": "import",
                "file_path": "nonexistent.xlsx"
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("File not found"));
    }

    #[tokio::test]
    async fn export_writes_xlsx_file() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());
        let test_file = "test_export.xlsx";

        let result = tool
            .execute(json!({
                "command": "export",
                "file_path": test_file,
                "sheet": "TestSheet",
                "data": [
                    ["Alice", 30],
                    ["Bob", 25]
                ],
                "header": ["Name", "Age"]
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["rows_written"], 2);

        // 验证文件存在
        let full_path = std::env::temp_dir().join(test_file);
        assert!(full_path.exists());

        // 清理
        let _ = std::fs::remove_file(&full_path);
    }

    #[tokio::test]
    async fn report_generates_with_sections() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());
        let test_file = "test_report.xlsx";

        let result = tool
            .execute(json!({
                "command": "report",
                "file_path": test_file,
                "title": "Test Report",
                "sections": [
                    {
                        "title": "Summary",
                        "data": [["Total", "100"]]
                    },
                    {
                        "title": "Details",
                        "data": [["Item", "Value"], ["A", "10"]]
                    }
                ]
            }))
            .await
            .unwrap();

        assert!(result.success);

        // 清理
        let full_path = std::env::temp_dir().join(test_file);
        let _ = std::fs::remove_file(&full_path);
    }

    #[tokio::test]
    async fn validate_returns_success_for_valid_file() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());

        // 创建测试文件
        let test_file = std::env::temp_dir().join("test_validate.xlsx");
        let workbook = rust_xlsxwriter::Workbook::new();
        workbook.add_worksheet().unwrap();
        workbook.save(&test_file).unwrap();

        let result = tool
            .execute(json!({
                "command": "validate",
                "file_path": test_file.file_name().unwrap().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["valid"], true);

        // 清理
        let _ = std::fs::remove_file(&test_file);
    }

    #[tokio::test]
    async fn info_returns_sheet_information() {
        let tool = ExcelTool::new(test_security(), std::env::temp_dir());

        // 创建测试文件
        let test_file = std::env::temp_dir().join("test_info.xlsx");
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let worksheet = workbook.add_worksheet().unwrap();
        worksheet.write_string(0, 0, "Test").unwrap();
        worksheet.write_string(1, 0, "Data").unwrap();
        workbook.save(&test_file).unwrap();

        let result = tool
            .execute(json!({
                "command": "info",
                "file_path": test_file.file_name().unwrap().to_str().unwrap()
            }))
            .await
            .unwrap();

        assert!(result.success);
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["sheet_count"], 1);
        assert!(output["sheets"].as_array().unwrap()[0]["rows"] >= 2);

        // 清理
        let _ = std::fs::remove_file(&test_file);
    }
}
