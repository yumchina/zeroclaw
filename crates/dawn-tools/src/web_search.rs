use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

pub struct DawnWebSearchTool {
    boot_token: Option<String>,
    base_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    config_path: PathBuf,
    secrets_encrypt: bool,
}

impl DawnWebSearchTool {
    pub fn new(
        boot_token: Option<String>,
        base_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> Self {
        Self {
            boot_token,
            base_url,
            max_results: max_results.clamp(1, 10),
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
                "Failed to read config file {} for Dawn web search token: {e}",
                self.config_path.display()
            )
        })?;

        let config: zeroclaw_config::schema::Config = toml::from_str(&contents).map_err(|e| {
            anyhow::anyhow!(
                "Failed to parse config file {} for Dawn web search token: {e}",
                self.config_path.display()
            )
        })?;

        // Resolve: [dawn.web_search].token → [dawn].token
        let raw_key = config
            .dawn
            .web_search
            .token
            .as_deref()
            .filter(|k| !k.is_empty())
            .or_else(|| config.dawn.token.as_deref().filter(|k| !k.is_empty()))
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Dawn web search token not configured (neither [dawn.web_search].token nor [dawn].token set)"))?;

        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store = zeroclaw_config::secrets::SecretStore::new(dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Dawn web search token not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    fn parse_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String> {
        let data = json.get("data").and_then(|d| d.as_array()).ok_or_else(|| {
            anyhow::anyhow!("Invalid Yumc-Search API response: missing or invalid data field")
        })?;

        if data.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let results = data[0]
            .get("results")
            .and_then(|r| r.as_array())
            .ok_or_else(|| {
                anyhow::anyhow!("Invalid Yumc-Search API response: missing results array")
            })?;

        if results.is_empty() {
            return Ok(format!("No results found for: {}", query));
        }

        let mut lines = vec![format!("Search results for: {} (via Yumc-Search)", query)];
        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let name = result
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let snippet = result.get("snippet").and_then(|s| s.as_str()).unwrap_or("");
            lines.push(format!("{}. {}", i + 1, name));
            lines.push(format!("   {}", url));
            if !snippet.is_empty() {
                lines.push(format!("   {}", snippet));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.resolve_token()?;
        let url = self
            .base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Dawn web search base URL not configured"))?;

        let body = json!({
            "queries": [query],
            "count": self.max_results,
        });

        let builder = reqwest::Client::builder().timeout(Duration::from_secs(self.timeout_secs));
        let builder = zeroclaw_config::schema::apply_runtime_proxy_to_builder(
            builder,
            "tool.dawn_web_search",
        );
        let client = builder.build()?;

        let response = client
            .post(url)
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Yumc-Search failed with status: {}", response.status());
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_results(&json, query)
    }
}

tool_attribution!(
    DawnWebSearchTool,
    ::zeroclaw_api::attribution::ToolKind::Search
);

#[async_trait]
impl Tool for DawnWebSearchTool {
    fn name(&self) -> &str {
        "dawn_web_search"
    }

    fn description(&self) -> &str {
        "Search the enterprise knowledge base using the internal Yumc-Search API. \
         Returns relevant results with titles, URLs, and descriptions from internal sources. \
         Use this for searching internal documentation, enterprise resources, or private knowledge bases."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

        if query.trim().is_empty() {
            anyhow::bail!("Search query cannot be empty");
        }

        match self.search(query).await {
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
    use serde_json::json;
    use std::path::PathBuf;

    fn make_tool() -> DawnWebSearchTool {
        DawnWebSearchTool::new(
            None,
            Some("http://example.com/search".into()),
            3,
            10,
            PathBuf::new(),
            false,
        )
    }

    #[test]
    fn tool_name_is_dawn_web_search() {
        assert_eq!(make_tool().name(), "dawn_web_search");
    }

    #[test]
    fn parse_results_formats_output_correctly() {
        let tool = make_tool();
        let json = json!({
            "data": [{
                "results": [
                    {"name": "Title 1", "url": "https://a.com", "snippet": "Snippet 1"},
                    {"name": "Title 2", "url": "https://b.com", "snippet": ""}
                ]
            }]
        });
        let result = tool.parse_results(&json, "test query").unwrap();
        assert!(result.contains("test query"));
        assert!(result.contains("Title 1"));
        assert!(result.contains("https://a.com"));
        assert!(result.contains("Snippet 1"));
        assert!(result.contains("Title 2"));
        assert!(result.contains("https://b.com"));
    }

    #[test]
    fn parse_results_empty_data_returns_no_results_message() {
        let tool = make_tool();
        let json = json!({"data": []});
        let result = tool.parse_results(&json, "my query").unwrap();
        assert!(result.contains("No results found"));
    }

    #[test]
    fn parse_results_missing_data_key_returns_error() {
        let tool = make_tool();
        let json = json!({"wrong_key": []});
        assert!(tool.parse_results(&json, "q").is_err());
    }

    #[test]
    fn resolve_token_uses_plaintext_boot_token() {
        let tool = DawnWebSearchTool::new(
            Some("plain-key".into()),
            Some("http://example.com/search".into()),
            3,
            10,
            PathBuf::new(),
            false,
        );
        assert_eq!(tool.resolve_token().unwrap(), "plain-key");
    }

    #[test]
    fn resolve_token_missing_returns_error() {
        let tool = DawnWebSearchTool::new(
            None,
            Some("http://example.com/search".into()),
            3,
            10,
            PathBuf::new(),
            false,
        );
        assert!(tool.resolve_token().is_err());
    }

    #[test]
    fn max_results_clamps_to_10() {
        let tool = DawnWebSearchTool::new(None, None, 100, 10, PathBuf::new(), false);
        assert_eq!(tool.max_results, 10);
    }
}
