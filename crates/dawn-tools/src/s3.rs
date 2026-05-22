//! Upload local files to a Dawn S3-compatible storage endpoint.
//!
//! The tool generates a unique remote path under `assistant/<uuid>.<ext>`,
//! POSTs the file as a multipart form to `{endpoint}/v1/assistant/file/upload`,
//! and returns the `{name, path, base_url}` triplet so the model can render
//! a download URL to the user.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncReadExt;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum file size for upload (100 MB).
const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

pub struct DawnS3Tool {
    security: Arc<SecurityPolicy>,
    endpoint: String,
    token: String,
}

impl DawnS3Tool {
    pub fn new(security: Arc<SecurityPolicy>, endpoint: String, token: String) -> Self {
        Self {
            security,
            endpoint,
            token,
        }
    }

    fn upload_url(&self) -> String {
        format!(
            "{}/v1/assistant/file/upload",
            self.endpoint.trim_end_matches('/')
        )
    }

    async fn do_upload(
        &self,
        path: &str,
        file_name: &str,
        content: Vec<u8>,
        content_type: &str,
    ) -> anyhow::Result<String> {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                ::serde_json::json!({
                    "path": path,
                    "file_name": file_name,
                    "content_type": content_type,
                    "content_size": content.len(),
                })
            ),
            "dawn_s3: do_upload called"
        );

        let url = format!(
            "{}?type=chat&path={}",
            self.upload_url(),
            urlencoding::encode(path)
        );

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"url": url})),
            "dawn_s3: upload URL prepared"
        );

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let part = reqwest::multipart::Part::bytes(content)
            .file_name(file_name.to_string())
            .mime_str(content_type)
            .map_err(|e| anyhow::Error::msg(format!("invalid mime type: {e}")))?;

        let form = reqwest::multipart::Form::new().part("file", part);

        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Send)
                .with_attrs(::serde_json::json!({"endpoint": self.endpoint})),
            "dawn_s3: sending HTTP POST"
        );

        let response = client
            .post(&url)
            .header("X-Assistant-Token", &self.token)
            .multipart(form)
            .send()
            .await?;

        let status = response.status();
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Receive)
                .with_attrs(::serde_json::json!({"status": status.as_u16()})),
            "dawn_s3: HTTP response received"
        );

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"status": status.as_u16(), "body": body})),
                "dawn_s3: upload failed"
            );
            anyhow::bail!("Upload failed with status {}: {}", status, body);
        }

        let json: serde_json::Value = response.json().await?;
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"json": json})),
            "dawn_s3: response body"
        );

        let remote_path = json.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "dawn_s3: response missing 'path' field"
            );
            anyhow::Error::msg("Invalid response: missing 'path' field")
        })?;

        let base_url = format!("{}/v1", self.endpoint.trim_end_matches('/'));
        let result = serde_json::json!({
            "name": file_name,
            "path": remote_path,
            "base_url": base_url,
        });
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(::serde_json::json!({"result": result})),
            "dawn_s3: upload success"
        );
        Ok(result.to_string())
    }
}

fn guess_content_type(path: &str) -> String {
    let path = Path::new(path);
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("zip") => "application/zip",
        Some("tar") => "application/x-tar",
        Some("gz" | "gzip") => "application/gzip",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        Some("avi") => "video/x-msvideo",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
    .to_string()
}

tool_attribution!(DawnS3Tool, ::zeroclaw_api::attribution::ToolKind::DawnS3);

#[async_trait]
impl Tool for DawnS3Tool {
    fn name(&self) -> &str {
        "dawn_s3"
    }

    fn description(&self) -> &str {
        "Upload a local file to Dawn S3 compatible storage. The remote path is auto-generated as `assistant/<uuid>.<ext>`. Returns the full download URL."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Local file path to upload (e.g., /Users/name/file.png)"
                },
                "content_type": {
                    "type": "string",
                    "description": "MIME type of the file (auto-detected from extension if not provided)"
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Invoke)
                .with_attrs(::serde_json::json!({"args": args})),
            "dawn_s3: execute called"
        );

        if !self.security.can_act() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "dawn_s3: action blocked, autonomy is read-only"
            );
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                "dawn_s3: action blocked, rate limit exceeded"
            );
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure),
                    "dawn_s3: missing required parameter file_path"
                );
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: file_path".into()),
                });
            }
        };

        // Resolve the user-supplied path relative to the workspace.
        let full_path = self.security.resolve_tool_path(file_path);

        // Open the file first so we have a stable handle. Then canonicalize
        // for validation. Reading from the already-open handle closes the
        // TOCTOU window between path check and file read.
        let mut file = match tokio::fs::File::open(&full_path).await {
            Ok(f) => f,
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": e.to_string()})),
                    "dawn_s3: file not found or inaccessible"
                );
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        json!({
                            "error": "File not found or inaccessible",
                            "suggestion": "Check the file path is correct and the file exists",
                        })
                        .to_string(),
                    ),
                });
            }
        };

        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": e.to_string()})),
                    "dawn_s3: failed to resolve path"
                );
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {e}")),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_path) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(
                        ::serde_json::json!({"resolved": resolved_path.display().to_string()})
                    ),
                "dawn_s3: path not allowed"
            );
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    self.security
                        .resolved_path_violation_message(&resolved_path),
                ),
            });
        }

        // Enforce file size limit to prevent OOM on large uploads.
        let meta = match file.metadata().await {
            Ok(m) => m,
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": e.to_string()})),
                    "dawn_s3: failed to read file metadata"
                );
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file metadata: {e}")),
                });
            }
        };

        let file_size = meta.len();
        if file_size > MAX_UPLOAD_BYTES {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "file_size": file_size,
                        "max": MAX_UPLOAD_BYTES,
                    })),
                "dawn_s3: file too large"
            );
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "File too large: {} bytes (max: {} MB)",
                    file_size,
                    MAX_UPLOAD_BYTES / (1024 * 1024)
                )),
            });
        }

        let content_type = args
            .get("content_type")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                let ct = guess_content_type(resolved_path.to_string_lossy().as_ref());
                ::zeroclaw_log::record!(
                    DEBUG,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({"content_type": ct})),
                    "dawn_s3: auto-detected content_type"
                );
                ct
            });

        let file_name = resolved_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .unwrap_or_else(|| {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "resolved": resolved_path.display().to_string(),
                        })),
                    "dawn_s3: non-UTF-8 filename, falling back to generic name"
                );
                "file".to_string()
            });

        let ext = resolved_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();

        let remote_path = format!("assistant/{}{}", uuid::Uuid::new_v4(), ext);
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"remote_path": remote_path})),
            "dawn_s3: remote_path generated"
        );

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Read).with_attrs(
                ::serde_json::json!({
                    "resolved": resolved_path.display().to_string(),
                    "file_size": file_size,
                })
            ),
            "dawn_s3: reading file"
        );
        let mut content = Vec::with_capacity(usize::try_from(file_size).unwrap_or(usize::MAX));
        if let Err(e) = file.read_to_end(&mut content).await {
            ::zeroclaw_log::record!(
                ERROR,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"err": e.to_string()})),
                "dawn_s3: failed to read file"
            );
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to read file: {e}")),
            });
        }

        match self
            .do_upload(&remote_path, &file_name, content, &content_type)
            .await
        {
            Ok(download_url) => Ok(ToolResult {
                success: true,
                output: download_url,
                error: None,
            }),
            Err(e) => {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"err": e.to_string()})),
                    "dawn_s3: upload failed"
                );
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::autonomy::AutonomyLevel;

    #[test]
    fn upload_url_format() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = DawnS3Tool::new(
            security,
            "http://172.20.48.84:8091".to_string(),
            "dawn_yumclaw".to_string(),
        );
        assert_eq!(
            tool.upload_url(),
            "http://172.20.48.84:8091/v1/assistant/file/upload"
        );
    }

    #[test]
    fn upload_url_trailing_slash() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = DawnS3Tool::new(
            security,
            "http://172.20.48.84:8091/".to_string(),
            "dawn_yumclaw".to_string(),
        );
        assert_eq!(
            tool.upload_url(),
            "http://172.20.48.84:8091/v1/assistant/file/upload"
        );
    }

    #[test]
    fn guess_content_type_images() {
        assert_eq!(guess_content_type("/path/to/image.png"), "image/png");
        assert_eq!(guess_content_type("/path/to/image.jpg"), "image/jpeg");
        assert_eq!(guess_content_type("/path/to/image.jpeg"), "image/jpeg");
        assert_eq!(guess_content_type("/path/to/image.gif"), "image/gif");
        assert_eq!(guess_content_type("/path/to/image.webp"), "image/webp");
    }

    #[test]
    fn guess_content_type_documents() {
        assert_eq!(guess_content_type("/path/to/file.pdf"), "application/pdf");
        assert_eq!(guess_content_type("/path/to/file.txt"), "text/plain");
        assert_eq!(guess_content_type("/path/to/file.html"), "text/html");
        assert_eq!(guess_content_type("/path/to/file.json"), "application/json");
    }

    #[test]
    fn guess_content_type_unknown() {
        assert_eq!(
            guess_content_type("/path/to/file.unknown"),
            "application/octet-stream"
        );
        assert_eq!(
            guess_content_type("/path/to/file"),
            "application/octet-stream"
        );
    }

    #[tokio::test]
    async fn execute_missing_file_path() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = DawnS3Tool::new(
            security,
            "http://localhost:8091".into(),
            "test-token".into(),
        );
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Missing required parameter"));
    }

    #[tokio::test]
    async fn execute_readonly_autonomy_blocked() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = DawnS3Tool::new(
            security,
            "http://localhost:8091".into(),
            "test-token".into(),
        );
        let result = tool
            .execute(json!({"file_path": "/any/file.txt"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("read-only"));
    }

    #[tokio::test]
    async fn execute_file_not_found() {
        let security = Arc::new(SecurityPolicy::default());
        let tool = DawnS3Tool::new(
            security,
            "http://localhost:8091".into(),
            "test-token".into(),
        );
        let result = tool
            .execute(json!({"file_path": "/nonexistent/path/file.txt"}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("File not found"),
            "expected file-not-found, got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_path_outside_workspace_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("secret.txt");
        std::fs::write(&outside_file, b"sensitive data").unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: workspace.clone(),
            workspace_only: true,
            ..SecurityPolicy::default()
        });
        let tool = DawnS3Tool::new(
            security,
            "http://localhost:8091".into(),
            "test-token".into(),
        );
        let result = tool
            .execute(json!({"file_path": outside_file.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("not allowed") || err.contains("outside"),
            "expected path rejection, got: {err}"
        );
    }

    #[tokio::test]
    async fn execute_upload_success() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/assistant/file/upload"))
            .and(header("X-Assistant-Token", "test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "path": "assistant/abc-123.pptx"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let test_file = workspace.join("report.pptx");
        std::fs::write(&test_file, b"fake pptx content").unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let tool = DawnS3Tool::new(security, server.uri(), "test-token".into());
        let result = tool
            .execute(json!({"file_path": test_file.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(result.success, "error: {:?}", result.error);
        let out: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(out["name"], "report.pptx");
        assert_eq!(out["path"], "assistant/abc-123.pptx");
        assert_eq!(out["base_url"], format!("{}/v1", server.uri()));
    }

    #[tokio::test]
    async fn execute_upload_http_500() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Error"))
            .expect(1)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let test_file = workspace.join("data.json");
        std::fs::write(&test_file, b"{}").unwrap();

        let security = Arc::new(SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let tool = DawnS3Tool::new(security, server.uri(), "test-token".into());
        let result = tool
            .execute(json!({"file_path": test_file.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("500"), "expected 500 error, got: {err}");
    }

    #[tokio::test]
    async fn execute_file_too_large() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let test_file = workspace.join("huge.bin");

        let f = std::fs::File::create(&test_file).unwrap();
        f.set_len(MAX_UPLOAD_BYTES + 1).unwrap();
        drop(f);

        let security = Arc::new(SecurityPolicy {
            workspace_dir: workspace.clone(),
            ..SecurityPolicy::default()
        });
        let tool = DawnS3Tool::new(
            security,
            "http://localhost:8091".into(),
            "test-token".into(),
        );
        let result = tool
            .execute(json!({"file_path": test_file.to_str().unwrap()}))
            .await
            .unwrap();

        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(
            err.contains("too large") || err.contains("MB"),
            "expected size-limit error, got: {err}"
        );
    }
}
