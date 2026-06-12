use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

pub struct DawnCrawlTool {
    boot_token: Option<String>,
    base_url: Option<String>,
    timeout_secs: u64,
    config_path: PathBuf,
    secrets_encrypt: bool,
}

impl DawnCrawlTool {
    pub fn new(
        boot_token: Option<String>,
        base_url: Option<String>,
        timeout_secs: u64,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> Self {
        Self {
            boot_token,
            base_url,
            timeout_secs: timeout_secs.max(1),
            config_path,
            secrets_encrypt,
        }
    }

    fn resolve_token(&self) -> anyhow::Result<String> {
        if let Some(ref key) = self.boot_token {
            if !key.is_empty() && !zeroclaw_config::secrets::SecretStore::is_encrypted(key) {
                return Ok(key.clone());
            }
        }
        self.reload_token()
    }

    fn reload_token(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read config file {} for Dawn crawl token: {e}",
                self.config_path.display()
            )
        })?;

        let config: zeroclaw_config::schema::Config =
            toml::from_str(&contents).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse config file {} for Dawn crawl token: {e}",
                    self.config_path.display()
                )
            })?;

        // Resolve: [dawn.crawl].token → [dawn].token
        let raw_key = config
            .dawn
            .crawl
            .token
            .as_deref()
            .filter(|k| !k.is_empty())
            .or_else(|| config.dawn.token.as_deref().filter(|k| !k.is_empty()))
            .map(str::to_owned)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Dawn crawl token not configured \
                     (neither [dawn.crawl].token nor [dawn].token set)"
                )
            })?;

        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store = zeroclaw_config::secrets::SecretStore::new(dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Dawn crawl token not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    fn format_result(&self, json: &serde_json::Value, url: &str) -> String {
        // Try common text-content keys at the top level
        for key in ["content", "text", "markdown", "body"] {
            if let Some(text) = json.get(key).and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    return format!("Crawled content from {url}:\n{text}");
                }
            }
        }
        // Try inside a `data` array (same shape as the search response)
        if let Some(first) = json
            .get("data")
            .and_then(|d| d.as_array())
            .and_then(|a| a.first())
        {
            for key in ["content", "text", "markdown", "body"] {
                if let Some(text) = first.get(key).and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        return format!("Crawled content from {url}:\n{text}");
                    }
                }
            }
        }
        // Fallback: pretty-print the raw JSON so the LLM can still extract value
        format!(
            "Crawled result from {url}:\n{}",
            serde_json::to_string_pretty(json).unwrap_or_else(|_| json.to_string())
        )
    }

    async fn crawl(&self, url: &str) -> anyhow::Result<String> {
        let token = self.resolve_token()?;
        let endpoint = self
            .base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Dawn crawl base URL not configured"))?;

        let body = json!({ "urls": [url] });

        let builder = reqwest::Client::builder().timeout(Duration::from_secs(self.timeout_secs));
        let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(
            builder,
            "tool.dawn_crawl",
        );
        let client = builder.build()?;

        let response = client
            .post(endpoint)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Dawn crawl failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        Ok(self.format_result(&json, url))
    }
}

tool_attribution!(DawnCrawlTool, ::zeroclaw_api::attribution::ToolKind::FetchUrl);

#[async_trait]
impl Tool for DawnCrawlTool {
    fn name(&self) -> &str {
        "dawn_crawl"
    }

    fn description(&self) -> &str {
        "Fetch the full content of an internal URL via the Dawn crawl service. \
         Use for retrieving enterprise web pages or intranet resources that require \
         authenticated access. Returns the page text or raw JSON when no text field \
         is found."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to crawl"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: url"))?;

        if url.trim().is_empty() {
            anyhow::bail!("URL cannot be empty");
        }

        match self.crawl(url).await {
            Ok(output) => Ok(ToolResult {
                success: true,
                output,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> DawnCrawlTool {
        DawnCrawlTool::new(
            Some("plain-token".into()),
            Some("http://example.com/crawl".into()),
            20,
            PathBuf::new(),
            false,
        )
    }

    #[test]
    fn resolve_token_uses_plaintext_boot_token() {
        let tool = make_tool();
        assert_eq!(tool.resolve_token().unwrap(), "plain-token");
    }

    #[test]
    fn resolve_token_missing_returns_error() {
        let tool = DawnCrawlTool::new(None, None, 20, PathBuf::new(), false);
        assert!(tool.resolve_token().is_err());
    }

    #[test]
    fn format_result_extracts_content_key() {
        let tool = make_tool();
        let json = serde_json::json!({"content": "hello world"});
        let out = tool.format_result(&json, "http://example.com");
        assert!(out.contains("hello world"));
        assert!(out.contains("http://example.com"));
    }

    #[test]
    fn format_result_extracts_from_data_array() {
        let tool = make_tool();
        let json = serde_json::json!({"data": [{"text": "page text"}]});
        let out = tool.format_result(&json, "http://example.com");
        assert!(out.contains("page text"));
    }

    #[test]
    fn format_result_falls_back_to_raw_json() {
        let tool = make_tool();
        let json = serde_json::json!({"unknown_key": 42});
        let out = tool.format_result(&json, "http://example.com");
        assert!(out.contains("unknown_key"));
    }

    #[test]
    fn timeout_clamps_to_minimum_one() {
        let tool = DawnCrawlTool::new(None, None, 0, PathBuf::new(), false);
        assert_eq!(tool.timeout_secs, 1);
    }
}
