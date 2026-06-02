# yumc_search 迁移到 dawn-tools 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 yumc_search 从 `zeroclaw-tools` 的 `web_search` 工具中完全移除，迁移到 `crates/dawn-tools` 中作为独立的 `dawn_web_search_tool`，同时新增 `[dawn.web_search]` 配置节。

**Architecture:** 方案 A — 完全提取。yumc_search HTTP 请求与解析逻辑复制到 `dawn-tools/src/web_search.rs`，实现独立的 `DawnWebSearchTool`。`WebSearchConfig` 恢复为只含公开搜索引擎字段，新增 `DawnConfig { web_search: DawnWebSearchConfig }` 对应 `[dawn.web_search]` TOML 节。两个工具名称不同，可同时注册。

**Tech Stack:** Rust, async-trait, reqwest, serde/toml, zeroclaw-api Tool trait, zeroclaw-config Configurable macro, wiremock（测试）

---

## 文件清单

| 文件 | 操作 |
|------|------|
| `crates/zeroclaw-config/src/schema.rs` | 删除 WebSearchConfig 中 yumc 字段；新增 DawnWebSearchConfig、DawnConfig；Config 添加 dawn 字段 |
| `crates/zeroclaw-tools/src/web_search_provider_routing.rs` | 删除 YumcSearch 枚举变体、常量、match arm、测试函数 |
| `crates/zeroclaw-tools/src/web_search_tool.rs` | 删除 yumc 相关字段、4 个方法、match arm；new_with_config 减少 2 个参数；修正所有测试调用 |
| `crates/zeroclaw-runtime/src/tools/mod.rs` | WebSearchTool 初始化移除 2 个 yumc 参数；新增 DawnWebSearchTool 注册块 |
| `crates/dawn-tools/src/web_search.rs` | 新建 DawnWebSearchTool |
| `crates/dawn-tools/src/lib.rs` | 导出 DawnWebSearchTool |

---

## Task 1: 清理 schema.rs — 移除 WebSearchConfig 中的 yumc 字段

**Files:**
- Modify: `crates/zeroclaw-config/src/schema.rs`

- [ ] **Step 1: 删除 WebSearchConfig 中的两个 yumc 字段**

在 `WebSearchConfig` struct 中，删除以下内容（含注释、serde 属性、#[secret] 属性）：

```rust
    // 删除这整段（6 行）：
    /// Yumc-Search API key (required if search_provider is `"yumc-search"`)
    #[serde(default)]
    #[secret]
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    pub yumc_search_api_key: Option<String>,
    /// Yumc-Search base URL (required if search_provider is `"yumc-search"`)
    #[serde(default)]
    pub yumc_search_base_url: Option<String>,
```

- [ ] **Step 2: 删除 Default impl 中的 yumc 字段**

在 `impl Default for WebSearchConfig` 中删除：
```rust
            yumc_search_api_key: None,
            yumc_search_base_url: None,
```

- [ ] **Step 3: 更新 WebSearchConfig 的 provider 注释**

将 `search_provider` 字段的文档注释从：
```rust
    /// Search provider: "duckduckgo" (free), "brave" (requires API key), "tavily" (requires API key), "searxng" (self-hosted), or "jina" (requires API key)
```
改为（已移除 yumc-search）：
```rust
    /// Search provider: "duckduckgo" (free), "brave" (requires API key), "tavily" (requires API key), "searxng" (self-hosted), or "jina" (requires API key)
```
（注释本身已正确，无需改动。确认 yumc-search 未在注释中出现即可。）

- [ ] **Step 4: 编译验证 zeroclaw-config**

```bash
cargo check -p zeroclaw-config
```

期望输出：`Finished` 无错误。若报 "unused function" 等 warning 是正常的（后续步骤处理）。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-config/src/schema.rs
git commit -m "refactor(config): remove yumc_search fields from WebSearchConfig"
```

---

## Task 2: schema.rs — 新增 DawnWebSearchConfig、DawnConfig 和 Config.dawn 字段

**Files:**
- Modify: `crates/zeroclaw-config/src/schema.rs`

- [ ] **Step 1: 在 WebSearchConfig 的 Default impl 之后添加辅助函数和 DawnWebSearchConfig**

在文件中找到 `impl Default for WebSearchConfig` 的结束 `}` 之后，插入以下内容：

```rust
fn default_dawn_web_search_max_results() -> usize {
    2
}

fn default_dawn_web_search_timeout_secs() -> u64 {
    20
}

/// Dawn enterprise web search configuration (`[dawn.web_search]` section).
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "dawn-web-search"]
pub struct DawnWebSearchConfig {
    /// Enable the `dawn_web_search_tool`. Default: `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum results per search (1-10). Default: `2`.
    #[serde(default = "default_dawn_web_search_max_results")]
    pub max_results: usize,
    /// Request timeout in seconds. Default: `20`.
    #[serde(default = "default_dawn_web_search_timeout_secs")]
    pub timeout_secs: u64,
    /// Yumc-Search API key (required).
    #[serde(default)]
    #[secret]
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    pub yumc_search_api_key: Option<String>,
    /// Yumc-Search base URL (required), e.g. `"http://search.example.local/api/v1/search"`.
    #[serde(default)]
    pub yumc_search_base_url: Option<String>,
}

impl Default for DawnWebSearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_results: default_dawn_web_search_max_results(),
            timeout_secs: default_dawn_web_search_timeout_secs(),
            yumc_search_api_key: None,
            yumc_search_base_url: None,
        }
    }
}

/// Container for all Dawn SaaS tool configurations (`[dawn.*]`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnConfig {
    /// Dawn enterprise web search tool (`[dawn.web_search]`).
    #[serde(default)]
    pub web_search: DawnWebSearchConfig,
}
```

- [ ] **Step 2: 在 Config 结构体中添加 dawn 字段**

在 Config struct 中找到 `dawn_s3` 字段：
```rust
    /// Dawn S3 file-upload tool configuration (`[dawn_s3]`).
    #[serde(default)]
    #[nested]
    pub dawn_s3: DawnS3Config,
```

在其 **之后** 插入：
```rust
    /// Dawn SaaS tools configuration (`[dawn.*]`).
    #[serde(default)]
    pub dawn: DawnConfig,
```

- [ ] **Step 3: 编译验证**

```bash
cargo check -p zeroclaw-config
```

期望：无错误。

- [ ] **Step 4: 写一个快速的 schema 测试来验证 TOML 解析**

在 `schema.rs` 末尾的测试模块中（或找到现有测试模块），添加：

```rust
#[cfg(test)]
mod dawn_config_tests {
    use super::*;

    #[test]
    fn dawn_web_search_config_parses_from_toml() {
        let toml = r#"
[dawn.web_search]
enabled = true
max_results = 2
timeout_secs = 20
yumc_search_base_url = "http://search.example.local/api/v1/search"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.dawn.web_search.enabled);
        assert_eq!(config.dawn.web_search.max_results, 2);
        assert_eq!(config.dawn.web_search.timeout_secs, 20);
        assert_eq!(
            config.dawn.web_search.yumc_search_base_url.as_deref(),
            Some("http://search.example.local/api/v1/search")
        );
    }

    #[test]
    fn dawn_web_search_config_defaults_to_disabled() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.dawn.web_search.enabled);
        assert_eq!(config.dawn.web_search.max_results, 2);
        assert_eq!(config.dawn.web_search.timeout_secs, 20);
    }
}
```

- [ ] **Step 5: 运行测试（先看是否失败）**

```bash
cargo test -p zeroclaw-config dawn_config_tests
```

期望：测试通过（Config struct 已有完整定义，无需额外实现）。

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-config/src/schema.rs
git commit -m "feat(config): add DawnWebSearchConfig and DawnConfig for [dawn.web_search]"
```

---

## Task 3: 清理 web_search_provider_routing.rs — 移除 YumcSearch

**Files:**
- Modify: `crates/zeroclaw-tools/src/web_search_provider_routing.rs`

- [ ] **Step 1: 删除 YumcSearch 枚举变体**

在 `WebSearchProviderRoute` enum 中删除：
```rust
    YumcSearch,
```

- [ ] **Step 2: 删除常量**

删除：
```rust
const YUMC_SEARCH_PROVIDER: &str = "yumc-search";
```

- [ ] **Step 3: 删除 match arm**

在 `resolve_web_search_provider` 函数中删除：
```rust
        "yumc-search" | "yumc_search" | "yumcsearch" => WebSearchProviderResolution {
            route: WebSearchProviderRoute::YumcSearch,
            canonical_provider: YUMC_SEARCH_PROVIDER,
            used_fallback: false,
        },
```

- [ ] **Step 4: 更新注释**

将：
```rust
        // Known non-default model_providers: Brave, SearXNG, Tavily, Jina, YumcSearch.
```
改为：
```rust
        // Known non-default model_providers: Brave, SearXNG, Tavily, Jina.
```

- [ ] **Step 5: 删除测试函数 resolve_aliases_to_yumc_search**

在 `#[cfg(test)] mod tests` 中，删除整个 `resolve_aliases_to_yumc_search` 测试函数：
```rust
    #[test]
    fn resolve_aliases_to_yumc_search() {
        let yumc_aliases = ["yumc-search", "yumc_search", "yumcsearch"];
        for alias in yumc_aliases {
            let resolved = resolve_web_search_provider(alias);
            assert_eq!(resolved.route, WebSearchProviderRoute::YumcSearch);
            assert_eq!(resolved.canonical_provider, YUMC_SEARCH_PROVIDER);
            assert!(!resolved.used_fallback);
        }
    }
```

- [ ] **Step 6: 运行路由测试**

```bash
cargo test -p zeroclaw-tools web_search_provider_routing
```

期望：所有剩余测试通过（duckduckgo、brave、searxng、tavily、jina、unknown）。

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-tools/src/web_search_provider_routing.rs
git commit -m "refactor(tools): remove YumcSearch from web search provider routing"
```

---

## Task 4: 清理 web_search_tool.rs — 移除所有 yumc 代码

**Files:**
- Modify: `crates/zeroclaw-tools/src/web_search_tool.rs`

- [ ] **Step 1: 删除 struct 中的 yumc 字段**

在 `WebSearchTool` struct 中删除：
```rust
    /// Boot-time Yumc-Search key snapshot.
    boot_yumc_search_api_key: Option<String>,
    /// Boot-time Yumc-Search base URL.
    yumc_search_base_url: Option<String>,
```

- [ ] **Step 2: 更新 new() 函数体**

在 `new()` 的 `Self { ... }` 初始化块中删除：
```rust
            boot_yumc_search_api_key: None,
            yumc_search_base_url: None,
```

- [ ] **Step 3: 更新 new_with_config() 签名和函数体**

将签名中的两个参数删除：
```rust
        // 删除：
        yumc_search_api_key: Option<String>,
        yumc_search_base_url: Option<String>,
```

将函数体中的两个字段初始化删除：
```rust
        // 删除：
        boot_yumc_search_api_key: yumc_search_api_key,
        yumc_search_base_url,
```

新的 `new_with_config` 完整签名为：
```rust
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config(
        model_provider: String,
        brave_api_key: Option<String>,
        tavily_api_key: Option<String>,
        jina_api_key: Option<String>,
        searxng_instance_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> Self {
        Self {
            model_provider: model_provider.trim().to_lowercase(),
            boot_brave_api_key: brave_api_key,
            boot_tavily_api_key: tavily_api_key,
            boot_jina_api_key: jina_api_key,
            searxng_instance_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path,
            secrets_encrypt,
        }
    }
```

- [ ] **Step 4: 删除 4 个 yumc 方法**

删除以下 4 个方法的完整实现（从 `fn resolve_yumc_search_api_key` 到 `parse_yumc_search_results` 末尾的 `}`）：
- `fn resolve_yumc_search_api_key(&self) -> anyhow::Result<String>`
- `fn reload_yumc_search_api_key(&self) -> anyhow::Result<String>`
- `async fn search_yumc_search(&self, query: &str) -> anyhow::Result<String>`
- `fn parse_yumc_search_results(&self, json: &serde_json::Value, query: &str) -> anyhow::Result<String>`

- [ ] **Step 5: 删除 execute() 中的 YumcSearch match arm**

在 `execute()` 的 match 块中删除：
```rust
            WebSearchProviderRoute::YumcSearch => self.search_yumc_search(query).await?,
```

- [ ] **Step 6: 修正所有 new_with_config 测试调用**

使用文本替换，将测试中所有 `new_with_config` 调用从 7 个 Option 参数改为 5 个。

每处调用的模式（可能有多处）是：
```rust
WebSearchTool::new_with_config(
    "<provider>".to_string(),
    None,   // brave
    None,   // tavily
    None,   // jina
    None,   // searxng
    None,   // yumc_api  ← 删除这行
    None,   // yumc_base ← 删除这行
    5,
    15,
    config_path,
    false,
)
```

改为：
```rust
WebSearchTool::new_with_config(
    "<provider>".to_string(),
    None,
    None,
    None,
    None,
    5,
    15,
    config_path,
    false,
)
```

或带有实际值的变体（如 `Some(encrypted)` 作为第一个 API key），同样减少 2 个 None。

用 grep 确认修改完整：
```bash
grep -n "yumc" crates/zeroclaw-tools/src/web_search_tool.rs
```
期望：无输出。

- [ ] **Step 7: 编译 zeroclaw-tools**

```bash
cargo check -p zeroclaw-tools
```

期望：无错误。

- [ ] **Step 8: 运行 web_search_tool 测试**

```bash
cargo test -p zeroclaw-tools
```

期望：所有测试通过。

- [ ] **Step 9: Commit**

```bash
git add crates/zeroclaw-tools/src/web_search_tool.rs
git commit -m "refactor(tools): remove yumc_search from WebSearchTool"
```

---

## Task 5: 更新 runtime/tools/mod.rs — 修正 WebSearchTool 初始化

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/mod.rs`

- [ ] **Step 1: 删除 WebSearchTool::new_with_config 调用中的两个 yumc 参数**

找到：
```rust
        tool_arcs.push(Arc::new(WebSearchTool::new_with_config(
            root_config.web_search.search_provider.clone(),
            root_config.web_search.brave_api_key.clone(),
            root_config.web_search.tavily_api_key.clone(),
            root_config.web_search.jina_api_key.clone(),
            root_config.web_search.searxng_instance_url.clone(),
            root_config.web_search.yumc_search_api_key.clone(),
            root_config.web_search.yumc_search_base_url.clone(),
            root_config.web_search.max_results,
            root_config.web_search.timeout_secs,
            root_config.config_path.clone(),
            root_config.secrets.encrypt,
        )));
```

改为：
```rust
        tool_arcs.push(Arc::new(WebSearchTool::new_with_config(
            root_config.web_search.search_provider.clone(),
            root_config.web_search.brave_api_key.clone(),
            root_config.web_search.tavily_api_key.clone(),
            root_config.web_search.jina_api_key.clone(),
            root_config.web_search.searxng_instance_url.clone(),
            root_config.web_search.max_results,
            root_config.web_search.timeout_secs,
            root_config.config_path.clone(),
            root_config.secrets.encrypt,
        )));
```

- [ ] **Step 2: 编译验证 runtime**

```bash
cargo check -p zeroclaw-runtime
```

期望：无错误。

- [ ] **Step 3: Commit**

```bash
git add crates/zeroclaw-runtime/src/tools/mod.rs
git commit -m "refactor(runtime): remove yumc params from WebSearchTool init"
```

---

## Task 6: 创建 dawn-tools/src/web_search.rs（TDD）

**Files:**
- Create: `crates/dawn-tools/src/web_search.rs`

- [ ] **Step 1: 先写失败的单元测试（在新文件中）**

创建 `crates/dawn-tools/src/web_search.rs`，内容只含测试模块：

```rust
use std::path::PathBuf;

// ── 测试先行 ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn tool_name_is_dawn_web_search_tool() {
        assert_eq!(make_tool().name(), "dawn_web_search_tool");
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
    fn resolve_api_key_uses_plaintext_boot_key() {
        let tool = DawnWebSearchTool::new(
            Some("plain-key".into()),
            Some("http://example.com/search".into()),
            3, 10, PathBuf::new(), false,
        );
        assert_eq!(tool.resolve_api_key().unwrap(), "plain-key");
    }

    #[test]
    fn resolve_api_key_missing_returns_error() {
        let tool = DawnWebSearchTool::new(
            None,
            Some("http://example.com/search".into()),
            3, 10, PathBuf::new(), false,
        );
        // No boot key and no config file → error
        assert!(tool.resolve_api_key().is_err());
    }

    #[test]
    fn max_results_clamps_to_10() {
        let tool = DawnWebSearchTool::new(None, None, 100, 10, PathBuf::new(), false);
        assert_eq!(tool.max_results, 10);
    }
}
```

- [ ] **Step 2: 尝试编译（预期失败，因为 DawnWebSearchTool 未定义）**

```bash
cargo check -p dawn-tools
```

期望：编译错误 `cannot find type DawnWebSearchTool`。

- [ ] **Step 3: 实现 DawnWebSearchTool**

在 `crates/dawn-tools/src/web_search.rs` 的 tests 模块之前，添加完整实现：

```rust
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_api::tool_attribution;

pub struct DawnWebSearchTool {
    boot_yumc_search_api_key: Option<String>,
    yumc_search_base_url: Option<String>,
    pub(crate) max_results: usize,
    timeout_secs: u64,
    config_path: PathBuf,
    secrets_encrypt: bool,
}

impl DawnWebSearchTool {
    pub fn new(
        boot_key: Option<String>,
        base_url: Option<String>,
        max_results: usize,
        timeout_secs: u64,
        config_path: PathBuf,
        secrets_encrypt: bool,
    ) -> Self {
        Self {
            boot_yumc_search_api_key: boot_key,
            yumc_search_base_url: base_url,
            max_results: max_results.clamp(1, 10),
            timeout_secs: timeout_secs.max(1),
            config_path,
            secrets_encrypt,
        }
    }

    pub(crate) fn resolve_api_key(&self) -> anyhow::Result<String> {
        if let Some(ref key) = self.boot_yumc_search_api_key {
            if !key.is_empty()
                && !zeroclaw_config::secrets::SecretStore::is_encrypted(key)
            {
                return Ok(key.clone());
            }
        }
        self.reload_api_key()
    }

    fn reload_api_key(&self) -> anyhow::Result<String> {
        let contents = std::fs::read_to_string(&self.config_path).map_err(|e| {
            anyhow::anyhow!(
                "Failed to read config file {} for Dawn web search API key: {e}",
                self.config_path.display()
            )
        })?;

        let config: zeroclaw_config::schema::Config =
            toml::from_str(&contents).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse config file {} for Dawn web search API key: {e}",
                    self.config_path.display()
                )
            })?;

        let raw_key = config
            .dawn
            .web_search
            .yumc_search_api_key
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Yumc-Search API key not configured"))?;

        if zeroclaw_config::secrets::SecretStore::is_encrypted(&raw_key) {
            let dir = self.config_path.parent().unwrap_or_else(|| Path::new("."));
            let store =
                zeroclaw_config::secrets::SecretStore::new(dir, self.secrets_encrypt);
            let plaintext = store.decrypt(&raw_key)?;
            if plaintext.is_empty() {
                anyhow::bail!("Yumc-Search API key not configured (decrypted value is empty)");
            }
            Ok(plaintext)
        } else {
            Ok(raw_key)
        }
    }

    pub(crate) fn parse_results(
        &self,
        json: &serde_json::Value,
        query: &str,
    ) -> anyhow::Result<String> {
        let data = json
            .get("data")
            .and_then(|d| d.as_array())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid Yumc-Search API response: missing or invalid data field"
                )
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

        let mut lines = vec![format!(
            "Search results for: {} (via Yumc-Search)",
            query
        )];
        for (i, result) in results.iter().take(self.max_results).enumerate() {
            let name = result
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("No title");
            let url = result.get("url").and_then(|u| u.as_str()).unwrap_or("");
            let snippet = result
                .get("snippet")
                .and_then(|s| s.as_str())
                .unwrap_or("");
            lines.push(format!("{}. {}", i + 1, name));
            lines.push(format!("   {}", url));
            if !snippet.is_empty() {
                lines.push(format!("   {}", snippet));
            }
        }

        Ok(lines.join("\n"))
    }

    async fn search(&self, query: &str) -> anyhow::Result<String> {
        let api_key = self.resolve_api_key()?;
        let url = self
            .yumc_search_base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Yumc-Search base URL not configured"))?;

        let body = json!({
            "queries": [query],
            "count": self.max_results,
        });

        let builder =
            reqwest::Client::builder().timeout(Duration::from_secs(self.timeout_secs));
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
            anyhow::bail!(
                "Yumc-Search failed with status: {}",
                response.status()
            );
        }

        let json: serde_json::Value = response.json().await?;
        self.parse_results(&json, query)
    }
}

tool_attribution!(DawnWebSearchTool, ::zeroclaw_api::attribution::ToolKind::Search);

#[async_trait]
impl Tool for DawnWebSearchTool {
    fn name(&self) -> &str {
        "dawn_web_search_tool"
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
```

注意：`apply_runtime_proxy_to_builder` 可能需要检查实际函数名。在 `web_search_tool.rs` 中的调用是：
```rust
zeroclaw_config::schema::apply_runtime_proxy_to_builder(builder, "tool.web_search");
```
确认此函数在 `zeroclaw-config` 中公开导出后使用相同方式调用。

- [ ] **Step 4: 确认 dawn-tools Cargo.toml 有 toml 依赖**

检查 `crates/dawn-tools/Cargo.toml`：
```bash
grep "toml" crates/dawn-tools/Cargo.toml
```

若无输出，在 `[dependencies]` 中添加：
```toml
toml = { version = "0.8", default-features = false, features = ["parse"] }
```

（zeroclaw-tools 的 Cargo.toml 中有 toml，检查版本保持一致）

- [ ] **Step 5: 编译**

```bash
cargo check -p dawn-tools
```

期望：无错误。若有编译错误，根据提示调整类型签名（如 `apply_runtime_proxy_to_builder` 的实际参数类型）。

- [ ] **Step 6: 运行测试（验证通过）**

```bash
cargo test -p dawn-tools
```

期望：所有 6 个测试通过。

- [ ] **Step 7: Commit**

```bash
git add crates/dawn-tools/src/web_search.rs crates/dawn-tools/Cargo.toml
git commit -m "feat(dawn-tools): add DawnWebSearchTool with yumc-search implementation"
```

---

## Task 7: 更新 dawn-tools/src/lib.rs — 导出 DawnWebSearchTool

**Files:**
- Modify: `crates/dawn-tools/src/lib.rs`

- [ ] **Step 1: 添加 web_search 模块和导出**

将文件内容改为：
```rust
//! Tools integrating ZeroClaw with the Dawn SaaS.
//!
//! Currently exposes two tools:
//! - [`s3::DawnS3Tool`] — uploads files to a Dawn S3-compatible storage endpoint.
//! - [`web_search::DawnWebSearchTool`] — searches via the internal Yumc-Search API.

pub mod s3;
pub mod web_search;

pub use s3::DawnS3Tool;
pub use web_search::DawnWebSearchTool;
```

- [ ] **Step 2: 编译验证**

```bash
cargo check -p dawn-tools
```

期望：无错误。

- [ ] **Step 3: Commit**

```bash
git add crates/dawn-tools/src/lib.rs
git commit -m "feat(dawn-tools): export DawnWebSearchTool from lib.rs"
```

---

## Task 8: runtime/tools/mod.rs — 注册 DawnWebSearchTool

**Files:**
- Modify: `crates/zeroclaw-runtime/src/tools/mod.rs`

- [ ] **Step 1: 添加 DawnWebSearchTool import**

在文件顶部找到 `DawnS3Tool` 的 use 语句（类似 `use dawn_tools::DawnS3Tool;`），在其后添加：
```rust
use dawn_tools::DawnWebSearchTool;
```

- [ ] **Step 2: 在 DawnS3Tool 注册块之后添加 DawnWebSearchTool 注册块**

找到 DawnS3Tool 注册结束的 `}` 之后（仍在 `#[cfg(feature = "dawn-tools")]` 内或附近），添加：

```rust
    #[cfg(feature = "dawn-tools")]
    if root_config.dawn.web_search.enabled {
        let base_url = root_config.dawn.web_search.yumc_search_base_url.clone();
        if base_url.as_deref().unwrap_or("").is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "dawn.web_search: enabled but [dawn.web_search].yumc_search_base_url is empty, skipping registration"
            );
        } else {
            tool_arcs.push(Arc::new(DawnWebSearchTool::new(
                root_config.dawn.web_search.yumc_search_api_key.clone(),
                base_url,
                root_config.dawn.web_search.max_results,
                root_config.dawn.web_search.timeout_secs,
                root_config.config_path.clone(),
                root_config.secrets.encrypt,
            )));
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Register)
                    .with_outcome(::zeroclaw_log::EventOutcome::Success),
                "dawn.web_search: tool registered"
            );
        }
    }
```

- [ ] **Step 3: 编译完整 workspace**

```bash
cargo check
```

期望：无错误。

- [ ] **Step 4: 运行全量测试**

```bash
cargo test
```

期望：所有测试通过。

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-runtime/src/tools/mod.rs
git commit -m "feat(runtime): register DawnWebSearchTool from [dawn.web_search] config"
```

---

## Task 9: 验证端到端配置

**Files:** 无新文件，验证现有配置文件

- [ ] **Step 1: 更新 fixtures/v1.toml 中的 web_search 配置**

找到并更新（或添加）：
```toml
[web_search]
enabled = true
max_results = 5
provider = "duckduckgo"
timeout_secs = 15
```

注意：`WebSearchConfig` 的字段名是 `search_provider`（非 `provider`）。检查 serde rename 配置。若字段名为 `search_provider`，TOML 中应使用：
```toml
[web_search]
enabled = true
max_results = 5
search_provider = "duckduckgo"
timeout_secs = 15
```

- [ ] **Step 2: 验证 [dawn.web_search] TOML 解析正确**

写一个快速的验证脚本（或在已有测试中确认），用实际加密 key 的前缀验证 SecretStore 识别：

```bash
cargo test -p zeroclaw-config dawn_config_tests
```

期望：通过。

- [ ] **Step 3: 运行完整 cargo test**

```bash
cargo test
```

期望：全部通过，无 yumc 相关报错。

- [ ] **Step 4: 最终 commit**

```bash
git add fixtures/
git commit -m "chore: update fixtures to reflect web_search/dawn.web_search config split"
```
