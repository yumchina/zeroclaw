# URL 白名单 — 泄露检测与工具输出脱敏配置方案

> **创建日期**: 2025-06-12
> **项目**: ZeroClaw (LeakDetector + scrub_credentials)
> **方案**: config.toml 白名单 + URL strip 预处理

---

## 问题描述

ZeroClaw 有两个独立的凭证脱敏机制：

1. **`LeakDetector::scan()`** — 在频道响应返回前，扫描并脱敏凭证（`sanitize_channel_response`）
2. **`scrub_credentials()`** — 在工具输出传给 LLM 前，脱敏凭证（`loop_.rs`）

两者都会误报 URL 查询参数中的 token：

### 实际案例

```
用户请求瑞幸下单 → AI 调用 createOrder API → 返回:
payOrderQrCodeUrl: https://open.lkcoffee.com/transfer/qrcode?token=hgnD0jgCF63...

scrub_credentials 处理 → token="hgnD*[REDACTED]" (保留前 4 字符)
LLM 收到: 二维码 URL 已被脱敏，无法使用
用户收到: 不完整的 URL，无法支付
```

---

## 设计目标

1. **消除 URL 参数误报** — 白名单域名/URL 的 token 不触发泄露检测
2. **保留用户体验** — Clean 返回时用户获得原始完整 URL
3. **分级安全** — 白名单只跳过指定的检查方法，API key 检测等仍然生效
4. **可配置** — 通过 config.toml 管理白名单，无需修改代码

---

## 配置格式

```toml
# 最简单的配置：只需添加白名单条目
# sensitivity 默认为 0.7，无需显式设置

[[security.leak_detector.url_allowlist]]
domain = "*.lkcoffee.com"
description = "瑞幸咖啡下单链接"

[[security.leak_detector.url_allowlist]]
domain = "open.example.com"
url_pattern = "/transfer/qrcode*"
description = "示例转账服务"
```

---

## 核心流程

```mermaid
flowchart TD
    Start([scan content]) --> Mask[mask_whitelist_urls<br/>将白名单 URL 替换为占位符]

    Mask --> Detection[用 masked detection_content<br/>执行全部 7 种检查]

    Detection --> Restore[restore_allowlist_urls<br/>还原白名单 URL]
    Restore --> Result{patterns 为空?}

    Result -->|是| Clean[返回 LeakResult::Clean<br/>用户获得原始完整 content]
    Result -->|否| Detected[返回 LeakResult::Detected<br/>非白名单部分已 redacted，白名单 URL 保留]

    Clean --> End([结束])
    Detected --> End

    style Clean fill:#c8e6c9
    style Detected fill:#ffcdd2
    style Mask fill:#fff4e1
    style Restore fill:#fff4e1
```

---

## 方案场景分析

| 场景 | 白名单匹配 | 检测逻辑 | 结果 | 用户获得 |
|------|-----------|----------|------|----------|
| `https://open.lkcoffee.com/transfer/qrcode?token=xxx` | ✅ `*.lkcoffee.com` | URL 被 mask，不参与检测，最后 restore | Clean | 完整 URL |
| `sk-1234 在 https://lkcoffee.com/api?token=xxx` | ✅ `*.lkcoffee.com` | URL 被 mask，`sk-1234` 被检测 | Detected (API key) | redacted，URL 完整 |
| 同一 token 同时出现在文本和白名单 URL 中 | ✅ `*.lkcoffee.com` | 文本 token 被检测并替换，白名单 URL 通过 restore 强保留 | Detected | 文本 redacted，URL 完整 |
| `https://unknown.com/api?token=xxx` | ❌ 不匹配 | URL 保留，`token=xxx` 被检测 | Detected (token) | redacted |
| `postgres://user:pass@host` 无 URL | ❌ 无 URL | 正常检测 | Detected (DB URL) | redacted |

---

## 需要修改的文件

| 文件 | 修改内容 |
|------|----------|
| `crates/zeroclaw-config/src/schema.rs` | ✅ 已完成：新增 `LeakDetectorConfig`、`UrlAllowlistEntry`；在 `SecurityConfig` 添加 `leak_detector` 字段 |
| `crates/zeroclaw-runtime/src/security/leak_detector.rs` | ✅ 已完成：新增 `from_config()`、`mask_whitelist_urls()`/`restore_allowlist_urls()`、`scan()` 掩码-还原流程 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | ✅ 已完成：`LeakDetector::from_config()` + `TOOL_LOOP_LEAK_DETECTOR_CONFIG` task-local scope |
| `crates/zeroclaw-runtime/src/agent/loop_.rs` | ✅ 已完成：`scrub_credentials_with_allowlist()` + allowlist task-local + 移除日志预 strip |
| `crates/zeroclaw-runtime/src/agent/tool_execution.rs` | ✅ 已完成：读取 task-local allowlist 执行清洗 |

### 需要查看但不修改的文件

| 文件 | 目的 |
|------|------|
| `crates/zeroclaw-config/src/policy.rs` | 了解 SecurityPolicy 构造模式 |
| `crates/zeroclaw-runtime/src/security/mod.rs` | 了解 re-export 链 |

---

## 实现细节

### 1. schema.rs 新增结构体

```rust
/// 泄露检测器配置 (`[security.leak_detector]`)
#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "security.leak_detector"]
pub struct LeakDetectorConfig {
    #[serde(default = "default_leak_detector_sensitivity")]
    pub sensitivity: f64,

    #[serde(default)]
    pub url_allowlist: Vec<UrlAllowlistEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct UrlAllowlistEntry {
    pub domain: String,
    #[serde(default)]
    pub url_pattern: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn default_leak_detector_sensitivity() -> f64 { 0.7 }

impl Default for LeakDetectorConfig {
    fn default() -> Self {
        Self {
            sensitivity: 0.7,
            url_allowlist: Vec::new(),
        }
    }
}
```

### 2. schema.rs SecurityConfig 集成

```rust
#[prefix = "security"]
pub struct SecurityConfig {
    // ... 现有字段保持不变 ...

    /// 凭证泄露检测器配置
    #[serde(default)]
    #[nested]
    pub leak_detector: LeakDetectorConfig,
}
```

### 3. leak_detector.rs 修改

规则在构造时一次性编译为正则，匹配期不再重复编译。

```rust
#[derive(Debug, Clone)]
pub struct LeakDetector {
    sensitivity: f64,
    url_allowlist: Vec<AllowlistRule>,
}

/// 域名/路径 glob 在构造时编译为锚定正则，匹配只用 `is_match`。
#[derive(Debug, Clone)]
pub struct AllowlistRule {
    domain_re: Regex,
    path_re: Option<Regex>,
}

impl AllowlistRule {
    fn new(domain_pattern: &str, url_pattern: Option<&str>) -> Option<Self> {
        let domain_re = compile_glob(domain_pattern)?;
        let path_re = match url_pattern {
            Some(p) => Some(compile_glob(p)?),
            None => None,
        };
        Some(Self { domain_re, path_re })
    }
}

/// glob (`*` → `.*`) → 锚定正则；非法模式返回 `None`。
fn compile_glob(pattern: &str) -> Option<Regex> {
    let re_str = regex::escape(pattern).replace(r"\*", ".*");
    Regex::new(&format!("^{}$", re_str)).ok()
}

/// 唯一的规则构建入口：`from_config` 与 tool loop 共用，避免两处重复构造。
/// 非法 glob 条目记 warn 并跳过，不中断检测。
pub fn allowlist_from_config(config: &LeakDetectorConfig) -> Vec<AllowlistRule> {
    config.url_allowlist.iter()
        .filter_map(|entry| AllowlistRule::new(&entry.domain, entry.url_pattern.as_deref()))
        .collect()
}

impl LeakDetector {
    pub fn from_config(config: &LeakDetectorConfig) -> Self {
        Self {
            sensitivity: config.sensitivity.clamp(0.0, 1.0),
            url_allowlist: allowlist_from_config(config),
        }
    }
}

/// mask/restore 为公共函数：`scan` 与 `scrub_credentials_with_allowlist` 共用，
/// 占位符格式与正则只存在一处。
pub(crate) const URL_PLACEHOLDER_PREFIX: &str = "\u{0}ZCWLU_";
pub(crate) const URL_PLACEHOLDER_SUFFIX: &str = "\u{0}";

pub fn mask_allowlist_urls(content: &str, allowlist: &[AllowlistRule]) -> (String, Vec<String>) { /* … */ }
pub fn restore_allowlist_urls(content: &str, preserved_urls: &[String]) -> String { /* restore_re 用 OnceLock 缓存 */ }

fn url_matches_rule(url: &str, rule: &AllowlistRule) -> bool {
    if !rule.domain_re.is_match(extract_domain(url)) {
        return false;
    }
    if let Some(ref path_re) = rule.path_re {
        if !path_re.is_match(extract_path(url)) {
            return false;
        }
    }
    true
}
```

> 注：`glob_match` 已删除——glob → regex 的编译在 `AllowlistRule::new` / `compile_glob` 中一次性完成，匹配阶段不再编译正则。

> 注：`extract_domain` 对 `user:pass@host` 形式的 URL 提取出 `user` 段，故白名单域名规则不豁免此类 URL（它们通常由 `check_database_urls` 捕获）。

### 4. scan() 修改

```rust
pub fn scan(&self, content: &str) -> LeakResult {
    // 1. 白名单 URL 掩码
    let (detection_content, preserved_urls) = self.mask_whitelist_urls(content);

    // 2. 在 masked 文本上做检测与替换
    let mut patterns = Vec::new();
    let mut redacted = detection_content.clone();

    self.check_api_keys(&detection_content, &mut patterns, &mut redacted);
    self.check_aws_credentials(&detection_content, &mut patterns, &mut redacted);
    self.check_generic_secrets(&detection_content, &mut patterns, &mut redacted);
    self.check_private_keys(&detection_content, &mut patterns, &mut redacted);
    self.check_jwt_tokens(&detection_content, &mut patterns, &mut redacted);
    self.check_database_urls(&detection_content, &mut patterns, &mut redacted);
    self.check_high_entropy_tokens(&detection_content, &mut patterns, &mut redacted);

    // 3. 还原白名单 URL（强保证保留）
    let redacted = restore_allowlist_urls(&redacted, &preserved_urls);

    if patterns.is_empty() {
        LeakResult::Clean
    } else {
        LeakResult::Detected { patterns, redacted }
    }
}
```

**关键设计**：
- 使用 `mask_whitelist_urls` + `restore_allowlist_urls`，而非 strip
- 白名单 URL 不参与检测，但在输出阶段始终被完整恢复
- 即使同一 token 在非白名单文本中被检测并替换，白名单 URL 仍能完整保留

### 5. orchestrator 调用点修改

```rust
// orchestrator/mod.rs

// Before
let leak_detector = zeroclaw_runtime::security::LeakDetector::new();

// After
let leak_detector = zeroclaw_runtime::security::LeakDetector::from_config(
    &self.ctx.leak_detector_config()
);
```

---

## 测试策略

### 单元测试

```rust
#[test]
fn whitelist_url_stripped_not_detected() {
    let config = LeakDetectorConfig {
        url_allowlist: vec![UrlAllowlistEntry {
            domain: "*.lkcoffee.com".into(),
            url_pattern: None,
            description: Some("test".into()),
        }],
        ..Default::default()
    };
    let detector = LeakDetector::from_config(&config);
    // URL 被 strip 掉，token 不会触发检测
    let content = "https://open.lkcoffee.com/transfer?token=longtokenthatwouldmatch";
    let result = detector.scan(content);
    assert!(matches!(result, LeakResult::Clean));
}

#[test]
fn non_url_content_still_checked() {
    let config = LeakDetectorConfig {
        url_allowlist: vec![UrlAllowlistEntry {
            domain: "*.lkcoffee.com".into(),
            url_pattern: None,
            description: Some("test".into()),
        }],
        ..Default::default()
    };
    let detector = LeakDetector::from_config(&config);
    // API key 在非 URL 部分，仍然被检测
    let content = "sk-1234567890 在 https://lkcoffee.com/api?token=xxx";
    let result = detector.scan(content);
    assert!(matches!(result, LeakResult::Detected { .. }));
}

#[test]
fn non_whitelist_url_still_detected() {
    let config = LeakDetectorConfig::default(); // 空白名单
    let detector = LeakDetector::from_config(&config);
    let content = "https://unknown.com/api?token=longtokenthatwouldmatch";
    let result = detector.scan(content);
    assert!(matches!(result, LeakResult::Detected { .. }));
}

#[test]
fn domain_url_pattern_match() {
    let config = LeakDetectorConfig {
        url_allowlist: vec![UrlAllowlistEntry {
            domain: "open.example.com".into(),
            url_pattern: Some("/transfer/qrcode*".into()),
            description: None,
        }],
        ..Default::default()
    };
    let rules = allowlist_from_config(&config);
    let rule = &rules[0];
    assert!(url_matches_rule("https://open.example.com/transfer/qrcode?token=xxx", rule));
    assert!(!url_matches_rule("https://open.example.com/api/order?token=xxx", rule));
    assert!(!url_matches_rule("https://other.example.com/transfer/qrcode", rule));
}

#[test]
fn allowlist_from_config_skips_invalid_pattern() {
    // 非法 glob（如未闭合的 `[`）编译失败被跳过，有效条目保留
    let config = LeakDetectorConfig {
        url_allowlist: vec![
            UrlAllowlistEntry { domain: "valid.example.com".into(), url_pattern: None, description: None },
            UrlAllowlistEntry { domain: "[invalid".into(), url_pattern: None, description: None },
        ],
        ..Default::default()
    };
    assert_eq!(allowlist_from_config(&config).len(), 1);
}
```

### 集成测试

```rust
#[test]
fn luckin_coffee_url_not_redacted_with_whitelist() {
    let config = LeakDetectorConfig {
        url_allowlist: vec![UrlAllowlistEntry {
            domain: "*.lkcoffee.com".into(),
            url_pattern: None,
            description: None,
        }],
        ..Default::default()
    };
    let detector = LeakDetector::from_config(&config);

    let ai_response = "您的下单链接：https://open.lkcoffee.com/transfer/qrcode?token=hgnD0jgCF63vmdtP0ITJsnMYdQpvX3TE8qCZwUGfRjSUtq00ixZipnGKtmk7msol";

    let result = detector.scan(ai_response);
    assert!(matches!(result, LeakResult::Clean));
}
```

---

## scrub_credentials 修改

### 问题

`scrub_credentials()` 在 `crates/zeroclaw-runtime/src/agent/loop_.rs:251`，每次工具输出传给 LLM 前被调用（line 1895）。它会脱敏匹配正则的所有 KV 对，包括 URL 中的 `token=`。

### 方案

在 `run_tool_call_loop` 中通过 task-local 读取 `LeakDetectorConfig`，构建 allowlist，并在 `execute_one_tool` 中应用 `scrub_credentials_with_allowlist`（掩码→清洗→还原）。

同时将 `TOOL_LOOP_LEAK_DETECTOR_CONFIG` 在 orchestrator 入口通过 `.scope(...)` 注入，避免 `agent_turn → run_tool_call_loop → delegate` 的长参数透传链。

### 实现

修改 `scrub_credentials` 所在链路：

```rust
// loop_.rs 中日志保持原语义（不预 strip）
"output": scrub_credentials(&outcome.output)

// tool_execution.rs 中通过 task-local 应用白名单保留
let output = TOOL_LOOP_ALLOWLIST
    .try_with(|rules| scrub_credentials_with_allowlist(normalized_output, rules))
    .unwrap_or_else(|_| scrub_credentials(normalized_output));
```

### 需要修改的调用点

`loop_.rs` 中的所有 `scrub_credentials` 调用（~15 处）。

### 新增回归测试

```rust
#[test]
fn whitelist_url_token_preserved_when_same_token_detected_elsewhere() {
    // 同一个 sk- token 同时出现在普通文本与白名单 URL 查询参数中
    // 预期：文本 token 被 redacted，白名单 URL 完整保留
}
```

该测试用于覆盖“边界场景：同模式替换可能连带破坏白名单 URL”，确保实现满足强保证保留语义。

---

## 风险评估

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| 白名单域名被恶意利用 | 低 | 中 | 白名单域名由运维控制，仅允许已知安全域名 |
| 非法 glob 配置条目 | 低 | 低 | `allowlist_from_config` 记 warn 并跳过，其余规则照常生效 |
| 配置加载失败导致白名单丢失 | 低 | 高 | `Default::default()` 空白名单，降级为全量检测 |
| 性能影响（每次 scan 匹配白名单） | 极低 | 极低 | 正则在构造时编译一次，匹配只做 `is_match`，开销可忽略 |

---

## 向后兼容性

- **默认配置**：`url_allowlist = []`（空白名单），行为与旧版完全一致
- **sensitivity 默认值**：`0.7`，与旧版 `LeakDetector::new()` 一致
- **旧版 `LeakDetector::new()` 保留**：测试代码仍可使用
