# yumc_search 迁移到 dawn-tools 设计文档

**日期**: 2026-06-02  
**状态**: 已批准

## 背景

yumc_search 是 Dawn 平台私有搜索 API，当前混入通用 `web_search_tool` 中。需要将其迁移到 `crates/dawn-tools`，使公开搜索引擎与内部企业搜索职责分离。

同时恢复标准 `[web_search]` 配置（duckduckgo），并为 yumc_search 创建独立的 `[dawn.web_search]` 配置节。

## 目标配置

```toml
[web_search]
enabled = true
max_results = 5
provider = "duckduckgo"
timeout_secs = 15

[dawn.web_search]
enabled = true
max_results = 2
timeout_secs = 20
yumc_search_api_key = "enc2:e5758a2d938cf5e9f836c4cebc46dce0ebddcf16dda4fdd9da27d032e7090fb137d56d1f240871b115bba62a66fd87f7de831ef8057193bafa09eb72302c83f91c74"
yumc_search_base_url = "http://share-nextg-nexx-gray.prd.yumc.local/tool/294/api/v1/search"
```

## 架构

### 涉及文件

| 文件 | 操作 |
|------|------|
| `zeroclaw-config/src/schema.rs` | 删除 WebSearchConfig 中 yumc 字段；新增 DawnWebSearchConfig、DawnConfig；Config 添加 dawn 字段 |
| `zeroclaw-tools/src/web_search_provider_routing.rs` | 删除 YumcSearch 枚举变体、常量、match arm、测试 |
| `zeroclaw-tools/src/web_search_tool.rs` | 删除 yumc 相关字段、方法、测试；new_with_config 减少 2 个参数 |
| `crates/dawn-tools/src/web_search.rs` | 新建 DawnWebSearchTool |
| `crates/dawn-tools/src/lib.rs` | 导出 DawnWebSearchTool |
| `zeroclaw-runtime/src/tools/mod.rs` | 更新 WebSearchTool 初始化；新增 DawnWebSearchTool 注册块 |

### Config 结构（Rust）

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[prefix = "dawn-web-search"]
pub struct DawnWebSearchConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_dawn_web_search_max_results")]
    pub max_results: usize,           // 默认 2
    #[serde(default = "default_dawn_web_search_timeout_secs")]
    pub timeout_secs: u64,            // 默认 20
    #[serde(default)]
    #[secret]
    pub yumc_search_api_key: Option<String>,
    #[serde(default)]
    pub yumc_search_base_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DawnConfig {
    #[serde(default)]
    pub web_search: DawnWebSearchConfig,
}

// 在主 Config 结构中：
#[serde(default)]
pub dawn: DawnConfig,   // 对应 [dawn.*] TOML 子表
```

### DawnWebSearchTool 结构

```rust
pub struct DawnWebSearchTool {
    boot_yumc_search_api_key: Option<String>,
    yumc_search_base_url: Option<String>,
    max_results: usize,
    timeout_secs: u64,
    config_path: PathBuf,
    secrets_encrypt: bool,
}

impl Tool for DawnWebSearchTool {
    fn name(&self) -> &str { "dawn_web_search_tool" }
    fn description(&self) -> &str {
        "Search the enterprise knowledge base using the internal Yumc-Search API. ..."
    }
}
```

## 数据流

1. **启动**：runtime 从 `config.dawn.web_search` 构建 `DawnWebSearchTool`（含 boot_key 快照 + config_path）
2. **调用**：Agent 调用 `dawn_web_search_tool` → 懒加载解密 API key（优先内存快照，否则重读 config.toml + SecretStore 解密）
3. **请求**：POST `yumc_search_base_url`，bearer auth，body `{"queries": [query], "count": max_results}`
4. **解析**：提取 `data[0].results[].{name, url, snippet}`，返回格式化文本

## 注册条件

```rust
#[cfg(feature = "dawn-tools")]
if config.dawn.web_search.enabled {
    // 校验 base_url 非空 → WARN 跳过 or 注册
}
```

## 错误处理

| 场景 | 处理 |
|------|------|
| API key 未配置 | `Err("Yumc-Search API key not configured")` |
| base URL 未配置 | `Err("Yumc-Search base URL not configured")` |
| HTTP 非 2xx | `bail!("Yumc-Search failed with status: {}")` |
| JSON 解析失败 | `Err` + `zeroclaw_log::record!(ERROR, ...)` |
| 注册时 base URL 为空 | `WARN` 日志，跳过注册 |

## 测试

- yumc 相关现有测试从 `web_search_tool.rs` 迁移到 `dawn-tools/src/web_search.rs`
- `web_search_tool.rs` 中 `new_with_config` 测试调用：Option 参数从 6 个减为 4 个
- `web_search_provider_routing.rs`：删除 `resolve_aliases_to_yumc_search` 测试

## web_search_tool.rs 参数变化

`new_with_config` 删除后签名：

```rust
pub fn new_with_config(
    model_provider: String,
    brave_api_key: Option<String>,
    tavily_api_key: Option<String>,
    jina_api_key: Option<String>,
    searxng_instance_url: Option<String>,
    // yumc_search_api_key 已删除
    // yumc_search_base_url 已删除
    max_results: usize,
    timeout_secs: u64,
    config_path: PathBuf,
    secrets_encrypt: bool,
) -> Self
```
