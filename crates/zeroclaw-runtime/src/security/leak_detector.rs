//! Credential leak detection for outbound content.
//!
//! Scans outbound messages for potential credential leaks before they are sent,
//! preventing accidental exfiltration of API keys, tokens, passwords, and other
//! sensitive values.
//!
//! Contributed from RustyClaw (MIT licensed).

use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;
use zeroclaw_config::schema::LeakDetectorConfig;
#[cfg(test)]
use zeroclaw_config::schema::UrlAllowlistEntry;

/// Minimum token length considered for high-entropy detection.
const ENTROPY_TOKEN_MIN_LEN: usize = 24;

/// Result of leak detection.
#[derive(Debug, Clone)]
pub enum LeakResult {
    /// No leaks detected.
    Clean,
    /// Potential leaks detected with redacted versions.
    Detected {
        /// Descriptions of detected leak patterns.
        patterns: Vec<String>,
        /// Content with sensitive values redacted.
        redacted: String,
    },
}

/// Credential leak detector for outbound content.
#[derive(Debug, Clone)]
pub struct LeakDetector {
    /// Sensitivity threshold (0.0-1.0, higher = more aggressive detection).
    sensitivity: f64,
    /// URL allowlist rules for excluding URLs from detection.
    url_allowlist: Vec<AllowlistRule>,
}

/// URL allowlist rule for matching and excluding URLs.
///
/// Domain and path globs are compiled to anchored regexes once at
/// construction time, so matching never recompiles a regex.
#[derive(Debug, Clone)]
pub struct AllowlistRule {
    domain_re: Regex,
    path_re: Option<Regex>,
}

impl AllowlistRule {
    fn new(domain_pattern: &str, url_pattern: Option<&str>) -> Self {
        let domain_re = compile_glob(domain_pattern);
        let path_re = url_pattern.map(compile_glob);
        Self { domain_re, path_re }
    }
}

/// Compile a glob (`*` → `.*`) into an anchored regex.
///
/// `regex::escape` guarantees a valid regex literal, so this never fails.
fn compile_glob(pattern: &str) -> Regex {
    let re_str = regex::escape(pattern).replace(r"\*", ".*");
    Regex::new(&format!("^{}$", re_str)).expect("regex::escape output is always valid")
}

/// Build allowlist rules from configuration.
pub fn allowlist_from_config(config: &LeakDetectorConfig) -> Vec<AllowlistRule> {
    config
        .url_allowlist
        .iter()
        .map(|entry| AllowlistRule::new(&entry.domain, entry.url_pattern.as_deref()))
        .collect()
}

impl Default for LeakDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// URL placeholder marker for preserving allowlisted URLs during scanning.
pub(crate) const URL_PLACEHOLDER_PREFIX: &str = "\u{0}ZCWLU_";
pub(crate) const URL_PLACEHOLDER_SUFFIX: &str = "\u{0}";

/// Mask allowlisted URLs and return masked content + preserved URL list.
pub fn mask_allowlist_urls(content: &str, allowlist: &[AllowlistRule]) -> (String, Vec<String>) {
    if allowlist.is_empty() {
        return (content.to_string(), Vec::new());
    }

    static URL_RE: OnceLock<Regex> = OnceLock::new();
    let url_re = URL_RE.get_or_init(|| Regex::new(r"https?://[^\s]+").unwrap());

    let mut masked = String::with_capacity(content.len());
    let mut preserved_urls: Vec<String> = Vec::new();
    let mut last_end = 0;

    for url_match in url_re.find_iter(content) {
        let url = url_match.as_str();
        if url_matches_any_rule(url, allowlist) {
            masked.push_str(&content[last_end..url_match.start()]);
            preserved_urls.push(url.to_string());
            masked.push_str(&format!(
                "{URL_PLACEHOLDER_PREFIX}{}{URL_PLACEHOLDER_SUFFIX}",
                preserved_urls.len() - 1
            ));
            last_end = url_match.end();
        }
    }

    masked.push_str(&content[last_end..]);
    (masked, preserved_urls)
}

/// Restore previously masked allowlisted URLs.
pub fn restore_allowlist_urls(content: &str, preserved_urls: &[String]) -> String {
    if preserved_urls.is_empty() {
        return content.to_string();
    }

    static RESTORE_RE: OnceLock<Regex> = OnceLock::new();
    let restore_re = RESTORE_RE.get_or_init(|| {
        Regex::new(&format!(
            "{}(\\d+){}",
            regex::escape(URL_PLACEHOLDER_PREFIX),
            regex::escape(URL_PLACEHOLDER_SUFFIX)
        ))
        .expect("restore regex is a valid static pattern")
    });

    restore_re
        .replace_all(content, |caps: &regex::Captures| {
            let idx: usize = caps[1].parse().unwrap_or(usize::MAX);
            preserved_urls.get(idx).cloned().unwrap_or_default()
        })
        .to_string()
}

impl LeakDetector {
    /// Create a new leak detector with default sensitivity.
    pub fn new() -> Self {
        Self {
            sensitivity: 0.7,
            url_allowlist: Vec::new(),
        }
    }

    /// Create a detector with custom sensitivity.
    pub fn with_sensitivity(sensitivity: f64) -> Self {
        Self {
            sensitivity: sensitivity.clamp(0.0, 1.0),
            url_allowlist: Vec::new(),
        }
    }

    /// Create a detector from configuration.
    pub fn from_config(config: &LeakDetectorConfig) -> Self {
        Self {
            sensitivity: config.sensitivity.clamp(0.0, 1.0),
            url_allowlist: allowlist_from_config(config),
        }
    }

    /// Mask whitelist URLs with placeholders for detection purposes.
    fn mask_whitelist_urls(&self, content: &str) -> (String, Vec<String>) {
        mask_allowlist_urls(content, &self.url_allowlist)
    }

    /// Scan content for potential credential leaks.
    pub fn scan(&self, content: &str) -> LeakResult {
        // 1. Mask whitelist URLs for detection and redaction safety
        let (detection_content, preserved_urls) = self.mask_whitelist_urls(content);

        // 2. Run all checks on masked content; redact on masked content as well
        let mut patterns = Vec::new();
        let mut redacted = detection_content.clone();

        self.check_api_keys(&detection_content, &mut patterns, &mut redacted);
        self.check_aws_credentials(&detection_content, &mut patterns, &mut redacted);
        self.check_generic_secrets(&detection_content, &mut patterns, &mut redacted);
        self.check_private_keys(&detection_content, &mut patterns, &mut redacted);
        self.check_jwt_tokens(&detection_content, &mut patterns, &mut redacted);
        self.check_database_urls(&detection_content, &mut patterns, &mut redacted);
        self.check_high_entropy_tokens(&detection_content, &mut patterns, &mut redacted);

        // 3. Restore original whitelist URLs before returning
        let redacted = restore_allowlist_urls(&redacted, &preserved_urls);

        if patterns.is_empty() {
            LeakResult::Clean
        } else {
            LeakResult::Detected { patterns, redacted }
        }
    }

    /// Check for common API key patterns.
    fn check_api_keys(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        static API_KEY_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = API_KEY_PATTERNS.get_or_init(|| {
            vec![
                // Stripe
                (
                    Regex::new(r"sk_(live|test)_[a-zA-Z0-9]{24,}").unwrap(),
                    "Stripe secret key",
                ),
                (
                    Regex::new(r"pk_(live|test)_[a-zA-Z0-9]{24,}").unwrap(),
                    "Stripe publishable key",
                ),
                // OpenAI
                (
                    Regex::new(r"sk-[a-zA-Z0-9]{20,}T3BlbkFJ[a-zA-Z0-9]{20,}").unwrap(),
                    "OpenAI API key",
                ),
                (
                    Regex::new(r"sk-[a-zA-Z0-9]{48,}").unwrap(),
                    "OpenAI-style API key",
                ),
                // Anthropic
                (
                    Regex::new(r"sk-ant-[a-zA-Z0-9-_]{32,}").unwrap(),
                    "Anthropic API key",
                ),
                // Google
                (
                    Regex::new(r"AIza[a-zA-Z0-9_-]{35}").unwrap(),
                    "Google API key",
                ),
                // GitHub
                (
                    Regex::new(r"gh[pousr]_[a-zA-Z0-9]{36,}").unwrap(),
                    "GitHub token",
                ),
                (
                    Regex::new(r"github_pat_[a-zA-Z0-9_]{22,}").unwrap(),
                    "GitHub PAT",
                ),
                // Generic
                (
                    Regex::new(r#"api[_-]?key[=:]\s*['"]*[a-zA-Z0-9_-]{20,}"#).unwrap(),
                    "Generic API key",
                ),
            ]
        });

        for (regex, name) in regexes {
            if regex.is_match(content) {
                patterns.push(String::from(*name));
                *redacted = regex
                    .replace_all(redacted, "[REDACTED_API_KEY]")
                    .to_string();
            }
        }
    }

    /// Check for AWS credentials.
    fn check_aws_credentials(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        static AWS_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = AWS_PATTERNS.get_or_init(|| {
            vec![
                (
                    Regex::new(r"AKIA[A-Z0-9]{16}").unwrap(),
                    "AWS Access Key ID",
                ),
                (
                    Regex::new(
                        r#"aws[_-]?secret[_-]?access[_-]?key[=:]\s*['"]*[a-zA-Z0-9/+=]{40}"#,
                    )
                    .unwrap(),
                    "AWS Secret Access Key",
                ),
            ]
        });

        for (regex, name) in regexes {
            if regex.is_match(content) {
                patterns.push(String::from(*name));
                *redacted = regex
                    .replace_all(redacted, "[REDACTED_AWS_CREDENTIAL]")
                    .to_string();
            }
        }
    }

    /// Check for generic secret patterns.
    fn check_generic_secrets(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        static SECRET_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = SECRET_PATTERNS.get_or_init(|| {
            vec![
                (
                    Regex::new(r#"(?i)password[=:]\s*['"]*[^\s'"]{8,}"#).unwrap(),
                    "Password in config",
                ),
                (
                    Regex::new(r#"(?i)secret[=:]\s*['"]*[a-zA-Z0-9_-]{16,}"#).unwrap(),
                    "Secret value",
                ),
                (
                    Regex::new(r#"(?i)token[=:]\s*['"]*[a-zA-Z0-9_.-]{20,}"#).unwrap(),
                    "Token value",
                ),
            ]
        });

        for (regex, name) in regexes {
            if regex.is_match(content) && self.sensitivity > 0.5 {
                patterns.push(String::from(*name));
                *redacted = regex.replace_all(redacted, "[REDACTED_SECRET]").to_string();
            }
        }
    }

    /// Check for private keys.
    fn check_private_keys(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        // PEM-encoded private keys
        let key_patterns = [
            (
                "-----BEGIN RSA PRIVATE KEY-----",
                "-----END RSA PRIVATE KEY-----",
                "RSA private key",
            ),
            (
                "-----BEGIN EC PRIVATE KEY-----",
                "-----END EC PRIVATE KEY-----",
                "EC private key",
            ),
            (
                "-----BEGIN PRIVATE KEY-----",
                "-----END PRIVATE KEY-----",
                "Private key",
            ),
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----",
                "-----END OPENSSH PRIVATE KEY-----",
                "OpenSSH private key",
            ),
        ];

        for (begin, end, name) in key_patterns {
            if content.contains(begin) && content.contains(end) {
                patterns.push(name.to_string());
                // Redact the entire key block
                if let Some(start_idx) = content.find(begin)
                    && let Some(end_idx) = content.find(end)
                {
                    let key_block = &content[start_idx..end_idx + end.len()];
                    *redacted = redacted.replace(key_block, "[REDACTED_PRIVATE_KEY]");
                }
            }
        }
    }

    /// Check for JWT tokens.
    fn check_jwt_tokens(&self, content: &str, patterns: &mut Vec<String>, redacted: &mut String) {
        static JWT_PATTERN: OnceLock<Regex> = OnceLock::new();
        let regex = JWT_PATTERN.get_or_init(|| {
            // JWT: three base64url-encoded parts separated by dots
            Regex::new(r"eyJ[a-zA-Z0-9_-]*\.eyJ[a-zA-Z0-9_-]*\.[a-zA-Z0-9_-]*").unwrap()
        });

        if regex.is_match(content) {
            patterns.push("JWT token".to_string());
            *redacted = regex.replace_all(redacted, "[REDACTED_JWT]").to_string();
        }
    }

    /// Check for database connection URLs.
    fn check_database_urls(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        static DB_PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
        let regexes = DB_PATTERNS.get_or_init(|| {
            vec![
                (
                    Regex::new(r"postgres(ql)?://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "PostgreSQL connection URL",
                ),
                (
                    Regex::new(r"mysql://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "MySQL connection URL",
                ),
                (
                    Regex::new(r"mongodb(\+srv)?://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "MongoDB connection URL",
                ),
                (
                    Regex::new(r"redis://[^:]+:[^@]+@[^\s]+").unwrap(),
                    "Redis connection URL",
                ),
            ]
        });

        for (regex, name) in regexes {
            if regex.is_match(content) {
                patterns.push(String::from(*name));
                *redacted = regex
                    .replace_all(redacted, "[REDACTED_DATABASE_URL]")
                    .to_string();
            }
        }
    }

    /// Check for high-entropy tokens that may be leaked credentials.
    ///
    /// Extracts candidate tokens from content (after stripping URLs to avoid
    /// false-positives on path segments) and flags any that exceed the Shannon
    /// entropy threshold derived from the detector's sensitivity.
    fn check_high_entropy_tokens(
        &self,
        content: &str,
        patterns: &mut Vec<String>,
        redacted: &mut String,
    ) {
        // Entropy threshold scales with sensitivity: at 0.7 this is ~4.37.
        let entropy_threshold = 3.5 + self.sensitivity * 1.25;

        // Strip URLs and media markers before extracting tokens so that path
        // segments are not mistaken for high-entropy credentials.
        // Media markers like [IMAGE:/path/to/file.png] contain filesystem paths
        // that look like high-entropy tokens when `/` is included in the token
        // character set (#4604).
        static URL_PATTERN: OnceLock<Regex> = OnceLock::new();
        let url_re = URL_PATTERN.get_or_init(|| Regex::new(r"https?://\S+").unwrap());
        static MEDIA_MARKER_PATTERN: OnceLock<Regex> = OnceLock::new();
        let media_re = MEDIA_MARKER_PATTERN.get_or_init(|| {
            Regex::new(r"\[(IMAGE|VIDEO|VOICE|AUDIO|DOCUMENT|FILE):[^\]]*\]").unwrap()
        });
        // Tool receipts (zc-receipt-...) are runtime-generated HMAC tokens that
        // intentionally appear in output. Strip them before entropy scanning so
        // they are not redacted as leaked credentials. See #4830.
        static RECEIPT_PATTERN: OnceLock<Regex> = OnceLock::new();
        let receipt_re =
            RECEIPT_PATTERN.get_or_init(|| Regex::new(r"zc-receipt-\d+-[A-Za-z0-9_-]+").unwrap());
        let content_stripped = url_re.replace_all(content, "");
        let content_without_urls = media_re.replace_all(&content_stripped, "");
        let content_without_receipts = receipt_re.replace_all(&content_without_urls, "");

        let tokens = extract_candidate_tokens(&content_without_receipts);

        for token in tokens {
            if token.len() >= ENTROPY_TOKEN_MIN_LEN {
                let entropy = shannon_entropy(token);
                if entropy >= entropy_threshold && has_mixed_alpha_digit(token) {
                    patterns.push("High-entropy token".to_string());
                    *redacted = redacted.replace(token, "[REDACTED_HIGH_ENTROPY_TOKEN]");
                }
            }
        }
    }
}

/// Extract candidate tokens by splitting on characters outside the
/// alphanumeric + common credential character set.
fn extract_candidate_tokens(content: &str) -> Vec<&str> {
    content
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-' && c != '+' && c != '/')
        .filter(|s| !s.is_empty())
        .collect()
}

/// Compute Shannon entropy (bits per character) for the given string.
fn shannon_entropy(s: &str) -> f64 {
    let len = s.len() as f64;
    if len == 0.0 {
        return 0.0;
    }
    let mut freq: HashMap<u8, usize> = HashMap::new();
    for &b in s.as_bytes() {
        *freq.entry(b).or_insert(0) += 1;
    }
    freq.values().fold(0.0, |acc, &count| {
        let p = count as f64 / len;
        acc - p * p.log2()
    })
}

/// Check whether a token contains both alphabetic and digit characters.
fn has_mixed_alpha_digit(s: &str) -> bool {
    let has_alpha = s.bytes().any(|b| b.is_ascii_alphabetic());
    let has_digit = s.bytes().any(|b| b.is_ascii_digit());
    has_alpha && has_digit
}

// ── URL allowlist helpers ──────────────────────────────────────────

fn url_matches_rule(url: &str, rule: &AllowlistRule) -> bool {
    if !rule.domain_re.is_match(extract_domain(url)) {
        return false;
    }
    match &rule.path_re {
        Some(re) => re.is_match(extract_path(url)),
        None => true,
    }
}

/// Extract the host portion of a URL.
///
/// Note: for `user:pass@host` authority URLs the extracted value is the
/// `user` segment, so allowlist domain rules do not exempt such URLs (they
/// are typically caught by `check_database_urls` instead).
fn extract_domain(url: &str) -> &str {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
}

fn extract_path(url: &str) -> &str {
    let after_scheme = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if let Some(slash_pos) = after_scheme.find('/') {
        let path_with_query = &after_scheme[slash_pos..];
        path_with_query.split('?').next().unwrap_or("")
    } else {
        ""
    }
}

/// Check if a URL matches any rule in the allowlist.
pub fn url_matches_any_rule(url: &str, allowlist: &[AllowlistRule]) -> bool {
    allowlist.iter().any(|rule| url_matches_rule(url, rule))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_content_passes() {
        let detector = LeakDetector::new();
        let result = detector.scan("This is just some normal text");
        assert!(matches!(result, LeakResult::Clean));
    }

    #[test]
    fn detects_stripe_keys() {
        let detector = LeakDetector::new();
        let content = "My Stripe key is sk_test_1234567890abcdefghijklmnop";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("Stripe")));
                assert!(redacted.contains("[REDACTED"));
            }
            LeakResult::Clean => panic!("Should detect Stripe key"),
        }
    }

    #[test]
    fn detects_aws_credentials() {
        let detector = LeakDetector::new();
        let content = "AWS key: AKIAIOSFODNN7EXAMPLE";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, .. } => {
                assert!(patterns.iter().any(|p| p.contains("AWS")));
            }
            LeakResult::Clean => panic!("Should detect AWS key"),
        }
    }

    #[test]
    fn detects_private_keys() {
        let detector = LeakDetector::new();
        let content = r#"
-----BEGIN RSA PRIVATE KEY-----
MIIEowIBAAKCAQEA0ZPr5JeyVDonXsKhfq...
-----END RSA PRIVATE KEY-----
"#;
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("private key")));
                assert!(redacted.contains("[REDACTED_PRIVATE_KEY]"));
            }
            LeakResult::Clean => panic!("Should detect private key"),
        }
    }

    #[test]
    fn detects_jwt_tokens() {
        let detector = LeakDetector::new();
        let content = "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("JWT")));
                assert!(redacted.contains("[REDACTED_JWT]"));
            }
            LeakResult::Clean => panic!("Should detect JWT"),
        }
    }

    #[test]
    fn detects_database_urls() {
        let detector = LeakDetector::new();
        let content = "DATABASE_URL=postgres://user:secretpassword@localhost:5432/mydb";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, .. } => {
                assert!(patterns.iter().any(|p| p.contains("PostgreSQL")));
            }
            LeakResult::Clean => panic!("Should detect database URL"),
        }
    }

    #[test]
    fn low_sensitivity_skips_generic() {
        let detector = LeakDetector::with_sensitivity(0.3);
        let content = "secret=mygenericvalue123456";
        let result = detector.scan(content);
        // Low sensitivity should not flag generic secrets
        assert!(matches!(result, LeakResult::Clean));
    }

    #[test]
    fn url_path_segments_not_flagged() {
        let detector = LeakDetector::new();
        // URL with a long mixed-alphanumeric path segment that would previously
        // false-positive as a high-entropy token.
        let content =
            "See https://example.org/documents/2024-report-a1b2c3d4e5f6g7h8i9j0.pdf for details";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "URL path segments should not trigger high-entropy detection"
        );
    }

    #[test]
    fn url_with_long_path_not_redacted() {
        let detector = LeakDetector::new();
        let content = "Reference: https://gov.example.com/publications/research/2024-annual-fiscal-policy-review-9a8b7c6d5e4f3g2h1i0j.html";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Long URL paths should not be redacted"
        );
    }

    #[test]
    fn tool_receipts_not_redacted_as_high_entropy() {
        let detector = LeakDetector::new();
        let content = "The date is Fri Mar 27.\n\n[receipt: zc-receipt-1774608496-gzpEBuUIRYX1vd4fQl4oYkqhq4-GnoJDStmlYzvQiWA]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Tool receipts (zc-receipt-...) should not be redacted"
        );
    }

    #[test]
    fn media_markers_not_redacted_as_high_entropy() {
        let detector = LeakDetector::new();
        let content = "Here is the image: [IMAGE:/Users/matt/.zeroclaw/workspace/skills/image-gen/images/20260324_135911.png]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Local media markers should not be redacted"
        );
    }

    #[test]
    fn detects_high_entropy_token_outside_url() {
        let detector = LeakDetector::new();
        // A standalone high-entropy token (not in a URL) should still be detected.
        let content = "Found credential: aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("High-entropy")));
                assert!(redacted.contains("[REDACTED_HIGH_ENTROPY_TOKEN]"));
            }
            LeakResult::Clean => panic!("Should detect high-entropy token"),
        }
    }

    #[test]
    fn low_sensitivity_raises_entropy_threshold() {
        let detector = LeakDetector::with_sensitivity(0.3);
        // At low sensitivity the entropy threshold is higher (3.5 + 0.3*1.25 = 3.875).
        // A repetitive mixed token has low entropy and should not be flagged.
        let content = "token found: ab12ab12ab12ab12ab12ab12ab12ab12";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Low-entropy repetitive tokens should not be flagged"
        );
    }

    #[test]
    fn extract_candidate_tokens_splits_correctly() {
        let tokens = extract_candidate_tokens("foo.bar:baz qux-quux key=val");
        assert!(tokens.contains(&"foo"));
        assert!(tokens.contains(&"bar"));
        assert!(tokens.contains(&"baz"));
        assert!(tokens.contains(&"qux-quux"));
        // '=' is a delimiter, not part of tokens
        assert!(tokens.contains(&"key"));
        assert!(tokens.contains(&"val"));
    }

    #[test]
    fn media_marker_image_path_not_redacted() {
        let detector = LeakDetector::new();
        let content = "Here is your image: [IMAGE:/Users/matt/.zeroclaw/workspace/skills/image-gen/images/20260324_135911.png]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Media marker image paths should not trigger high-entropy detection"
        );
    }

    #[test]
    fn media_marker_video_not_redacted() {
        let detector = LeakDetector::new();
        let content = "Attached: [VIDEO:/path/to/long/video/file/name123456.mp4]";
        let result = detector.scan(content);
        assert!(
            matches!(result, LeakResult::Clean),
            "Media marker video paths should not trigger high-entropy detection"
        );
    }

    #[test]
    fn actual_high_entropy_still_detected() {
        let detector = LeakDetector::new();
        let content = "Leaked credential: aB3xK9mW2pQ7vL4nR8sT1yU6hD0jF5cG";
        let result = detector.scan(content);
        match result {
            LeakResult::Detected { patterns, redacted } => {
                assert!(patterns.iter().any(|p| p.contains("High-entropy")));
                assert!(redacted.contains("[REDACTED_HIGH_ENTROPY_TOKEN]"));
            }
            LeakResult::Clean => {
                panic!("Should still detect high-entropy tokens outside media markers")
            }
        }
    }

    #[test]
    fn shannon_entropy_empty_string() {
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn shannon_entropy_single_char() {
        // All same characters: entropy = 0
        assert_eq!(shannon_entropy("aaaa"), 0.0);
    }

    #[test]
    fn shannon_entropy_two_equal_chars() {
        // "ab" repeated: entropy = 1.0 bit
        let e = shannon_entropy("abab");
        assert!((e - 1.0).abs() < 0.001);
    }

    // ── URL allowlist tests ──────────────────────────────────

    #[test]
    fn mask_whitelist_urls_masks_only_allowlisted_urls() {
        let config = LeakDetectorConfig {
            url_allowlist: vec![UrlAllowlistEntry {
                domain: "*.lkcoffee.com".into(),
                url_pattern: None,
                description: None,
            }],
            ..Default::default()
        };
        let detector = LeakDetector::from_config(&config);
        let content = "Text https://open.lkcoffee.com/transfer?token=secret more text";
        let (masked, preserved_urls) = detector.mask_whitelist_urls(content);

        assert!(!masked.contains("https://open.lkcoffee.com/transfer?token=secret"));
        assert_eq!(preserved_urls, vec!["https://open.lkcoffee.com/transfer?token=secret"]);
    }

    #[test]
    fn mask_whitelist_urls_preserves_non_whitelist_url() {
        let config = LeakDetectorConfig {
            url_allowlist: vec![UrlAllowlistEntry {
                domain: "*.lkcoffee.com".into(),
                url_pattern: None,
                description: None,
            }],
            ..Default::default()
        };
        let detector = LeakDetector::from_config(&config);
        let content = "Text https://unknown.com/api?token=secret";
        let (masked, preserved_urls) = detector.mask_whitelist_urls(content);

        assert_eq!(masked, content);
        assert!(preserved_urls.is_empty());
    }

    #[test]
    fn whitelist_url_not_detected_as_leak() {
        let config = LeakDetectorConfig {
            url_allowlist: vec![UrlAllowlistEntry {
                domain: "*.lkcoffee.com".into(),
                url_pattern: None,
                description: None,
            }],
            ..Default::default()
        };
        let detector = LeakDetector::from_config(&config);
        let content = "https://open.lkcoffee.com/transfer/qrcode?token=hgnD0jgCF63vmdtP0ITJsnMYdQpvX3TE8qCZwUGfRjSUtq00ixZipnGKtmk7msol";
        let result = detector.scan(content);
        assert!(matches!(result, LeakResult::Clean));
    }

    #[test]
    fn whitelist_url_token_preserved_when_same_token_detected_elsewhere() {
        let config = LeakDetectorConfig {
            url_allowlist: vec![UrlAllowlistEntry {
                domain: "*.lkcoffee.com".into(),
                url_pattern: None,
                description: None,
            }],
            ..Default::default()
        };
        let detector = LeakDetector::from_config(&config);
        let token = "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz1234567890AB";
        let content = format!(
            "Leaked token: {token}\nSafe callback: https://open.lkcoffee.com/transfer/qrcode?token={token}"
        );

        let result = detector.scan(&content);

        match result {
            LeakResult::Detected { redacted, .. } => {
                assert!(redacted.contains("[REDACTED_API_KEY]"));
                assert!(redacted.contains(&format!(
                    "https://open.lkcoffee.com/transfer/qrcode?token={token}"
                )));
            }
            LeakResult::Clean => panic!("Should detect leaked token outside whitelist URL"),
        }
    }

    #[test]
    fn real_credential_outside_url_still_detected() {
        let config = LeakDetectorConfig {
            url_allowlist: vec![UrlAllowlistEntry {
                domain: "*.lkcoffee.com".into(),
                url_pattern: None,
                description: None,
            }],
            ..Default::default()
        };
        let detector = LeakDetector::from_config(&config);
        // Strip 要求 48+ 字符
        let content = "sk-1234567890abcdef1234567890abcdef1234567890abcdef1234567890 在 https://lkcoffee.com/api?token=xxx";
        let result = detector.scan(content);
        assert!(matches!(result, LeakResult::Detected { .. }));
    }

    #[test]
    fn non_whitelist_url_still_detected() {
        let config = LeakDetectorConfig::default();
        let detector = LeakDetector::from_config(&config);
        // token 参数值需要 >= 20 字符
        let content = "https://unknown.com/api?token=abcdefghijklmnopqrstuvwxyz";
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
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert!(url_matches_rule(
            "https://open.example.com/transfer/qrcode?token=xxx",
            rule
        ));
        assert!(!url_matches_rule(
            "https://open.example.com/api/order?token=xxx",
            rule
        ));
        assert!(!url_matches_rule(
            "https://other.example.com/transfer/qrcode",
            rule
        ));
    }

    #[test]
    fn allowlist_from_config_builds_rules() {
        let config = LeakDetectorConfig {
            url_allowlist: vec![
                UrlAllowlistEntry {
                    domain: "*.lkcoffee.com".into(),
                    url_pattern: None,
                    description: None,
                },
                UrlAllowlistEntry {
                    domain: "api.example.com".into(),
                    url_pattern: Some("/v1/*".into()),
                    description: None,
                },
            ],
            ..Default::default()
        };
        let rules = allowlist_from_config(&config);
        assert_eq!(rules.len(), 2);
        assert!(url_matches_rule(
            "https://open.lkcoffee.com/x?token=t",
            &rules[0]
        ));
        assert!(url_matches_rule(
            "https://api.example.com/v1/anything",
            &rules[1]
        ));
    }
}
