use async_trait::async_trait;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use zeroclaw_api::tool::{Tool, ToolResult};
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
        format!("{}/v1/assistant/file/upload", self.endpoint.trim_end_matches('/'))
    }

    async fn do_upload(
        &self,
        path: &str,
        file_name: &str,
        content: Vec<u8>,
        content_type: &str,
    ) -> anyhow::Result<String> {
        tracing::debug!(
            target: "dawn_s3",
            "do_upload called: path={}, file_name={}, content_type={}, content_size={}",
            path,
            file_name,
            content_type,
            content.len()
        );

        let url = format!(
            "{}?type=chat&path={}",
            self.upload_url(),
            urlencoding::encode(path)
        );

        tracing::debug!(target: "dawn_s3", "upload URL: {}", url);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let part = reqwest::multipart::Part::bytes(content)
            .file_name(file_name.to_string())
            .mime_str(content_type)
            .map_err(|e| anyhow::anyhow!("invalid mime type: {e}"))?;

        let form = reqwest::multipart::Form::new().part("file", part);

        tracing::debug!(target: "dawn_s3", "sending HTTP POST to {}", self.endpoint);

        let response = client
            .post(&url)
            .header("X-Assistant-Token", &self.token)
            .multipart(form)
            .send()
            .await?;

        let status = response.status();
        tracing::debug!(target: "dawn_s3", "HTTP response status: {}", status);

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            tracing::error!(target: "dawn_s3", "upload failed with status {}: {}", status, body);
            anyhow::bail!("Upload failed with status {}: {}", status, body);
        }

        let json: serde_json::Value = response.json().await?;
        tracing::debug!(target: "dawn_s3", "response JSON: {:?}", json);

        let remote_path = json
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                tracing::error!(target: "dawn_s3", "response missing 'path' field");
                anyhow::anyhow!("Invalid response: missing 'path' field")
            })?;

        let base_url = format!("{}/v1", self.endpoint.trim_end_matches('/'));
        let result = serde_json::json!({
            "name": file_name,
            "path": remote_path,
            "base_url": base_url
        });
        tracing::info!(target: "dawn_s3", "upload success, result: {}", result);
        Ok(result.to_string())
    }
}

fn guess_content_type(path: &str) -> String {
    let path = Path::new(path);
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("html") | Some("htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("zip") => "application/zip",
        Some("tar") => "application/x-tar",
        Some("gz") | Some("gzip") => "application/gzip",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        Some("avi") => "video/x-msvideo",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
    .to_string()
}

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
        tracing::debug!(target: "dawn_s3", "execute called with args: {:?}", args);

        if !self.security.can_act() {
            tracing::warn!(target: "dawn_s3", "action blocked: autonomy is read-only");
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            tracing::warn!(target: "dawn_s3", "action blocked: rate limit exceeded");
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                tracing::error!(target: "dawn_s3", "missing required parameter: file_path");
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing required parameter: file_path".into()),
                });
            }
        };

        // Security: resolve the tool path relative to the workspace.
        let full_path = self.security.resolve_tool_path(file_path);

        // Open file first so we have a stable handle. Then canonicalize the
        // original path for validation. Reading from the already-open handle
        // closes the TOCTOU window between path check and file read.
        let mut file = match tokio::fs::File::open(&full_path).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(target: "dawn_s3", "file not found or inaccessible: {}", e);
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(json!({
                        "error": "File not found or inaccessible",
                        "suggestion": "Check the file path is correct and the file exists",
                    }).to_string()),
                });
            }
        };

        // Validate the canonicalized path is within allowed roots.
        let resolved_path = match tokio::fs::canonicalize(&full_path).await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(target: "dawn_s3", "failed to resolve path: {}", e);
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to resolve file path: {}", e)),
                });
            }
        };

        if !self.security.is_resolved_path_allowed(&resolved_path) {
            tracing::warn!(target: "dawn_s3", "path not allowed: {}", resolved_path.display());
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(self.security.resolved_path_violation_message(&resolved_path)),
            });
        }

        // Enforce file size limit to prevent OOM on large uploads.
        let meta = match file.metadata().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(target: "dawn_s3", "failed to get file metadata: {}", e);
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to read file metadata: {}", e)),
                });
            }
        };

        let file_size = meta.len();
        if file_size > MAX_UPLOAD_BYTES {
            tracing::warn!(
                target: "dawn_s3",
                "file too large: {} bytes (max: {} bytes)",
                file_size,
                MAX_UPLOAD_BYTES
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
                tracing::debug!(target: "dawn_s3", "auto-detected content_type: {}", ct);
                ct
            });

        let file_name = resolved_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .unwrap_or_else(|| {
                tracing::warn!(
                    target: "dawn_s3",
                    "non-UTF-8 filename, falling back to generic name: {}",
                    resolved_path.display()
                );
                "file".to_string()
            });

        tracing::debug!(target: "dawn_s3", "file_name extracted: {}", file_name);

        let ext = resolved_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e))
            .unwrap_or_default();

        let remote_path = format!("assistant/{}{}", uuid::Uuid::new_v4(), ext);
        tracing::debug!(target: "dawn_s3", "remote_path generated: {}", remote_path);

        tracing::info!(target: "dawn_s3", "reading file: {} ({} bytes)", resolved_path.display(), file_size);
        let mut content = Vec::with_capacity(file_size as usize);
        if let Err(e) = file.read_to_end(&mut content).await {
            tracing::error!(target: "dawn_s3", "failed to read file: {}", e);
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to read file: {}", e)),
            });
        }
        tracing::debug!(target: "dawn_s3", "file read success, size: {} bytes", content.len());

        match self.do_upload(&remote_path, &file_name, content, &content_type).await {
            Ok(download_url) => {
                tracing::info!(target: "dawn_s3", "upload completed successfully");
                Ok(ToolResult {
                    success: true,
                    output: download_url,
                    error: None,
                })
            }
            Err(e) => {
                tracing::error!(target: "dawn_s3", "upload failed: {}", e);
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

    // ── execute() path tests ────────────────────────────────────────────

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
        let result = tool.execute(json!({"file_path": "/any/file.txt"})).await.unwrap();
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
        assert!(err.contains("File not found"), "expected file-not-found, got: {err}");
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
        let tool = DawnS3Tool::new(
            security,
            server.uri(),
            "test-token".into(),
        );
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
        let tool = DawnS3Tool::new(
            security,
            server.uri(),
            "test-token".into(),
        );
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

        // Create a sparse/allocation file larger than 100MB
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