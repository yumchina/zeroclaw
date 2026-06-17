# URL Allowlist Migration (PR #48 → 0.8.0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate `yumchina/zeroclaw` PR #48 (URL allowlist for leak detector + tool-output scrub) to the 0.8.0 branch, so that URLs matching configured domain/path globs are preserved verbatim (with their query-string tokens intact) by both `LeakDetector::scan()` and `scrub_credentials()`.

**Architecture:** Two-stage mask→detect→restore. Before any credential detection runs, URLs matching the allowlist are replaced with collision-free placeholders. The detector runs on the masked text. After detection, placeholders are restored to the original URLs. This keeps every existing detector pattern untouched and adds a single pre/post filter pair.

A `LeakDetectorConfig` is added under `[security.leak_detector]` carrying `sensitivity` and a `Vec<UrlAllowlistEntry>`. Compiled `AllowlistRule`s are propagated through tool-execution paths via two `tokio::task_local!` slots (following the existing `TOOL_LOOP_COST_TRACKING_CONTEXT` pattern in `crates/zeroclaw-runtime/src/agent/cost.rs:86`).

**Tech Stack:** Rust 2024, `regex`, `tokio::task_local!`, `serde`, existing `zeroclaw_config::nested` macro. No new dependencies.

**Reference materials:**
- PR #48 merge commit: `a5517b964` on `origin/master`
- Spec (in PR commit tree): `git show a5517b964:docs/superpowers/specs/2026-06-12-url-allowlist-design.md` (457 lines — read for matching semantics and edge-case rationale)
- Original plan: `git show a5517b964:docs/superpowers/plans/2026-06-12-url-allowlist.md`

## Global Constraints

- **Crate layout (0.8.0 differs from master):** `scrub_credentials` lives at `crates/zeroclaw-runtime/src/agent/turn/redact.rs:13`, NOT in `loop_.rs` as on master. There are **5 call sites** of `scrub_credentials` to update (vs 1 on master): `loop_.rs:422`, `tool_execution.rs:{100,199,223,261}`. There are **2 call sites** of `LeakDetector::new()`: `orchestrator/mod.rs:{3468, 9831}`.
- **Default behavior unchanged:** `SecurityConfig::leak_detector` default = empty allowlist → mask/restore is a no-op, every existing test must keep passing. This is a load-bearing invariant.
- **SecurityConfig pattern:** Use `#[serde(default)] #[nested] pub leak_detector: LeakDetectorConfig` — same shape as the existing `audit/otp/estop/nevis/webauthn` fields at `schema.rs:13995-14020`.
- **task_local pattern:** Follow `crates/zeroclaw-runtime/src/agent/cost.rs:86`. Type: `Option<Arc<Vec<AllowlistRule>>>`. Arc so we don't clone the rule list per tool call.
- **No new deps**, no new sandbox plumbing, no new channel APIs.
- **Commit cadence:** One commit per task. Conventional commits prefix `feat(security):` / `test(security):` / `refactor(security):` as appropriate.
- **TDD:** Every task starts with a failing test.
- **Comment policy (project default):** No "WHAT" or "where this is called from" comments. Only document non-obvious WHY (e.g. why mask placeholder must avoid `{` `}` collisions).

---

## File Structure

| File | Responsibility | Action |
|------|---------------|--------|
| `crates/zeroclaw-config/src/schema.rs` | Add `LeakDetectorConfig`, `UrlAllowlistEntry`, default fn, wire into `SecurityConfig` | Modify (insert near `SecurityConfig` at line ~13995) |
| `crates/zeroclaw-runtime/src/security/leak_detector.rs` | Add `AllowlistRule`, `compile_glob`, `url_matches_any_rule`, `mask_allowlist_urls`, `restore_allowlist_urls`, `allowlist_from_config`, `LeakDetector::from_config`. Update `scan()` to optionally use mask/restore. | Modify |
| `crates/zeroclaw-runtime/src/security/mod.rs` | Re-export new public items | Modify |
| `crates/zeroclaw-runtime/src/agent/turn/redact.rs` | Add `scrub_credentials_with_allowlist(input: &str, rules: &[AllowlistRule]) -> String` | Modify |
| `crates/zeroclaw-runtime/src/agent/scrub_context.rs` | **New file.** Owns the `tokio::task_local!` slots and small accessor helpers. Keeps `loop_.rs` from growing. | Create |
| `crates/zeroclaw-runtime/src/agent/mod.rs` | Add `pub mod scrub_context;` | Modify |
| `crates/zeroclaw-runtime/src/agent/loop_.rs` | Replace `scrub_credentials(raw)` at line 422 with allowlist-aware variant reading from `scrub_context` | Modify |
| `crates/zeroclaw-runtime/src/agent/tool_execution.rs` | Replace 4 `scrub_credentials(...)` calls with allowlist-aware variant | Modify |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | Replace 2 `LeakDetector::new()` with `from_config`; thread `config.security.leak_detector` into `sanitize_channel_response`; wrap per-message processing in `TOOL_LOOP_ALLOWLIST::scope(...)` | Modify |

---

## Task 1: Schema — `LeakDetectorConfig` + `UrlAllowlistEntry`

**Files:**
- Modify: `crates/zeroclaw-config/src/schema.rs:13995` (SecurityConfig and surrounding region)
- Test: same file (inline `#[cfg(test)]` mod, follow existing pattern)

**Interfaces:**
- Consumes: nothing from earlier tasks
- Produces:
  - `pub struct LeakDetectorConfig { pub sensitivity: f64, pub url_allowlist: Vec<UrlAllowlistEntry> }`
  - `pub struct UrlAllowlistEntry { pub domain: String, pub url_pattern: Option<String>, pub description: Option<String> }`
  - `pub fn default_leak_detector_sensitivity() -> f64` returning `0.7`
  - `SecurityConfig.leak_detector: LeakDetectorConfig` field

- [ ] **Step 1: Write the failing test**

Append to the existing inline test module in `schema.rs` (search for `mod security_config_tests` or the nearest SecurityConfig serde test; if none, add one near line 14020):

```rust
#[cfg(test)]
mod leak_detector_config_serde_tests {
    use super::*;

    #[test]
    fn security_leak_detector_defaults_to_empty_allowlist_and_default_sensitivity() {
        let toml = "[audit]\n";
        let cfg: SecurityConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.leak_detector.sensitivity, 0.7);
        assert!(cfg.leak_detector.url_allowlist.is_empty());
    }

    #[test]
    fn security_leak_detector_parses_url_allowlist_entries() {
        let toml = r#"
[leak_detector]
sensitivity = 0.5

[[leak_detector.url_allowlist]]
domain = "*.lkcoffee.com"
url_pattern = "/transfer/qrcode*"
description = "Luckin order links"
"#;
        let cfg: SecurityConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.leak_detector.sensitivity, 0.5);
        assert_eq!(cfg.leak_detector.url_allowlist.len(), 1);
        let e = &cfg.leak_detector.url_allowlist[0];
        assert_eq!(e.domain, "*.lkcoffee.com");
        assert_eq!(e.url_pattern.as_deref(), Some("/transfer/qrcode*"));
        assert_eq!(e.description.as_deref(), Some("Luckin order links"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p zeroclaw-config leak_detector_config_serde_tests -- --nocapture
```

Expected: FAIL with `no field "leak_detector" on type SecurityConfig` (or similar — neither struct nor field exists yet).

- [ ] **Step 3: Add the types and wire SecurityConfig**

Find `pub struct SecurityConfig {` at `schema.rs:13995`. After the existing `pub webauthn: WebAuthnConfig,` field (around line 14019), insert:

```rust
    /// Leak detector configuration: sensitivity + URL allowlist that
    /// preserves matching URLs verbatim through both `LeakDetector::scan`
    /// and `scrub_credentials_with_allowlist`.
    #[serde(default)]
    #[nested]
    pub leak_detector: LeakDetectorConfig,
```

Immediately after the closing `}` of `SecurityConfig` (line ~14020), insert:

```rust
/// `[security.leak_detector]` — controls credential detection sensitivity and
/// the URL allowlist used by mask→detect→restore in both
/// `LeakDetector::scan` and `scrub_credentials_with_allowlist`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LeakDetectorConfig {
    #[serde(default = "default_leak_detector_sensitivity")]
    pub sensitivity: f64,
    #[serde(default)]
    pub url_allowlist: Vec<UrlAllowlistEntry>,
}

impl Default for LeakDetectorConfig {
    fn default() -> Self {
        Self {
            sensitivity: default_leak_detector_sensitivity(),
            url_allowlist: Vec::new(),
        }
    }
}

/// One allowlist row. `domain` is a glob (`*` and `?`) matched against the
/// URL host. `url_pattern`, when set, is a glob matched against `path?query`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UrlAllowlistEntry {
    pub domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

fn default_leak_detector_sensitivity() -> f64 {
    0.7
}
```

Verify the `#[nested]` derive is available in scope. Search the file for an existing `#[nested]` usage (already present on `audit`/`otp` etc.) — no extra `use` needed.

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p zeroclaw-config leak_detector_config_serde_tests
```

Expected: both tests PASS.

- [ ] **Step 5: Full crate check (no schema-snapshot breakage)**

```bash
cargo test -p zeroclaw-config
```

Expected: 0 failures. If any "schema snapshot" / golden test fails, regenerate it per the failure message's instruction (typically `INSTA_UPDATE=auto cargo test -p zeroclaw-config` — verify in failure output). Add the regenerated snapshot to staging.

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-config/src/schema.rs crates/zeroclaw-config/src/snapshots/ 2>/dev/null
git commit -m "feat(security): add [security.leak_detector] config schema (PR #48)"
```

---

## Task 2: `AllowlistRule` compile + match

**Files:**
- Modify: `crates/zeroclaw-runtime/src/security/leak_detector.rs`
- Test: same file (existing `#[cfg(test)]` mod)

**Interfaces:**
- Consumes: (nothing — pure)
- Produces:
  - `pub struct AllowlistRule { domain_re: Regex, path_re: Option<Regex> }`
  - `impl AllowlistRule { pub fn new(domain_glob: &str, url_pattern: Option<&str>) -> Option<Self> }` (returns `None` on invalid pattern; caller logs)
  - `pub fn url_matches_any_rule(url: &str, rules: &[AllowlistRule]) -> bool`
  - Private `fn compile_glob(pattern: &str, anchor: bool) -> Option<Regex>` (handles `*` → `.*`, `?` → `.`, escapes everything else; `anchor` adds `^…$`)

- [ ] **Step 1: Write the failing tests**

Append at the end of the existing `#[cfg(test)] mod tests` in `leak_detector.rs`:

```rust
    #[test]
    fn allowlist_rule_matches_exact_domain() {
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        assert!(url_matches_any_rule(
            "https://api.example.com/v1/orders?token=abc",
            &[rule.clone()]
        ));
        assert!(!url_matches_any_rule("https://evil.com/x?token=abc", &[rule]));
    }

    #[test]
    fn allowlist_rule_wildcard_subdomain_matches() {
        let rule = AllowlistRule::new("*.lkcoffee.com", None).unwrap();
        assert!(url_matches_any_rule(
            "https://open.lkcoffee.com/x?token=abc",
            &[rule.clone()]
        ));
        assert!(url_matches_any_rule(
            "https://order.api.lkcoffee.com/x?token=abc",
            &[rule.clone()]
        ));
        assert!(!url_matches_any_rule(
            "https://lkcoffee.com.evil.com/x",
            &[rule]
        ));
    }

    #[test]
    fn allowlist_rule_path_filter_narrows_match() {
        let rule = AllowlistRule::new(
            "*.lkcoffee.com",
            Some("/transfer/qrcode*"),
        )
        .unwrap();
        assert!(url_matches_any_rule(
            "https://open.lkcoffee.com/transfer/qrcode?token=abc",
            &[rule.clone()]
        ));
        assert!(!url_matches_any_rule(
            "https://open.lkcoffee.com/admin/login?token=abc",
            &[rule]
        ));
    }

    #[test]
    fn allowlist_rule_invalid_pattern_returns_none() {
        // Trailing backslash creates an invalid regex after escape.
        assert!(AllowlistRule::new("[", None).is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p zeroclaw-runtime --lib security::leak_detector::tests::allowlist_
```

Expected: 4 tests, all FAIL with `cannot find type AllowlistRule in this scope` (or equivalent).

- [ ] **Step 3: Implement AllowlistRule**

Add at the top of `leak_detector.rs` (just after existing `use` statements):

```rust
use url::Url;
```

If `url` crate is not already in `zeroclaw-runtime`'s `Cargo.toml`, **stop and verify** — grep `crates/zeroclaw-runtime/Cargo.toml` for `^url\b`. If absent, search the workspace `Cargo.toml` `[workspace.dependencies]` for `url` — if present, add `url.workspace = true` to `crates/zeroclaw-runtime/Cargo.toml` `[dependencies]`. If absent there too, fall back to a hand-rolled URL parser:

```rust
// Hand-rolled URL split avoiding a new dep. Returns (host, path_query) or None.
fn split_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (host, path_query) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // Strip user-info `user@host`
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    // Strip `:port`
    let host = host.split(':').next().unwrap_or(host);
    Some((host, path_query))
}
```

Then add the rule type and helpers (regardless of whether `url` crate was added — gate usage to whichever side was kept):

```rust
/// One compiled allowlist entry. Use `AllowlistRule::new` to build, then
/// pass slices to `url_matches_any_rule` / `mask_allowlist_urls`.
#[derive(Debug, Clone)]
pub struct AllowlistRule {
    domain_re: Regex,
    path_re: Option<Regex>,
}

impl AllowlistRule {
    pub fn new(domain_glob: &str, url_pattern: Option<&str>) -> Option<Self> {
        let domain_re = compile_glob(domain_glob, true)?;
        let path_re = match url_pattern {
            Some(p) => Some(compile_glob(p, true)?),
            None => None,
        };
        Some(Self { domain_re, path_re })
    }
}

/// True iff `url` matches any rule in `rules`. A rule with no `path_re`
/// matches the whole URL on host alone; with `path_re` set, both must match.
pub fn url_matches_any_rule(url: &str, rules: &[AllowlistRule]) -> bool {
    let Some((host, path_query)) = split_url(url) else {
        return false;
    };
    rules.iter().any(|r| {
        r.domain_re.is_match(host)
            && r.path_re.as_ref().map_or(true, |p| p.is_match(path_query))
    })
}

fn compile_glob(pattern: &str, anchor: bool) -> Option<Regex> {
    let mut out = String::with_capacity(pattern.len() * 2 + 2);
    if anchor {
        out.push('^');
    }
    for ch in pattern.chars() {
        match ch {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                out.push('\\');
                out.push(c);
            }
        }
    }
    if anchor {
        out.push('$');
    }
    Regex::new(&out).ok()
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p zeroclaw-runtime --lib security::leak_detector::tests::allowlist_
```

Expected: 4 PASS.

- [ ] **Step 5: Run full crate test to catch regressions**

```bash
cargo test -p zeroclaw-runtime --lib security::leak_detector
```

Expected: all existing leak_detector tests still PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/zeroclaw-runtime/src/security/leak_detector.rs crates/zeroclaw-runtime/Cargo.toml 2>/dev/null
git commit -m "feat(security): AllowlistRule + glob compilation for URL allowlist (PR #48)"
```

---

## Task 3: Mask / restore + `LeakDetector::from_config`

**Files:**
- Modify: `crates/zeroclaw-runtime/src/security/leak_detector.rs`
- Modify: `crates/zeroclaw-runtime/src/security/mod.rs` (re-exports)
- Test: same `leak_detector.rs` tests module

**Interfaces:**
- Consumes: `AllowlistRule`, `url_matches_any_rule` (Task 2); `zeroclaw_config::schema::LeakDetectorConfig` (Task 1)
- Produces:
  - `pub fn allowlist_from_config(cfg: &LeakDetectorConfig) -> Vec<AllowlistRule>` (skips invalid entries with a `tracing::warn!`)
  - `pub fn mask_allowlist_urls(input: &str, rules: &[AllowlistRule]) -> (String, Vec<(String, String)>)` — returns (masked text, `(placeholder, original_url)` pairs)
  - `pub fn restore_allowlist_urls(masked: &str, mapping: &[(String, String)]) -> String`
  - `impl LeakDetector { pub fn from_config(cfg: &LeakDetectorConfig) -> Self }` — same shape as `new()` but using `cfg.sensitivity`. The detector itself does NOT carry the allowlist; callers mask first.
- **Placeholder format:** `«URL_ALLOWLIST_PLACEHOLDER_<N>»` where `N` is a monotonic index. Guillemets (U+00AB / U+00BB) are chosen because they don't appear in URLs, JSON, or credential regexes, and they survive `scrub_credentials`'s regex untouched.

- [ ] **Step 1: Write the failing tests**

Append to the leak_detector tests module:

```rust
    use zeroclaw_config::schema::{LeakDetectorConfig, UrlAllowlistEntry};

    #[test]
    fn mask_and_restore_roundtrips_to_original() {
        let rules = vec![AllowlistRule::new("api.example.com", None).unwrap()];
        let input = "Visit https://api.example.com/o?token=hgnD0jgCF63abc for QR.";
        let (masked, mapping) = mask_allowlist_urls(input, &rules);
        assert!(!masked.contains("hgnD0jgCF63abc"), "token leaked in masked text");
        assert_eq!(mapping.len(), 1);
        let restored = restore_allowlist_urls(&masked, &mapping);
        assert_eq!(restored, input);
    }

    #[test]
    fn scan_with_masked_allowlist_preserves_token() {
        let rules = vec![AllowlistRule::new("api.example.com", None).unwrap()];
        let input = "QR url: https://api.example.com/o?token=hgnD0jgCF63abcdefghij";
        let (masked, mapping) = mask_allowlist_urls(input, &rules);
        let detector = LeakDetector::from_config(&LeakDetectorConfig::default());
        let scan_result = detector.scan(&masked);
        let final_text = match scan_result {
            LeakResult::Clean => masked,
            LeakResult::Detected { redacted, .. } => redacted,
        };
        let restored = restore_allowlist_urls(&final_text, &mapping);
        assert!(
            restored.contains("token=hgnD0jgCF63abcdefghij"),
            "allowlisted URL token must survive scan: {restored}"
        );
    }

    #[test]
    fn allowlist_from_config_skips_invalid_rows() {
        let cfg = LeakDetectorConfig {
            sensitivity: 0.7,
            url_allowlist: vec![
                UrlAllowlistEntry {
                    domain: "[".into(), // invalid
                    url_pattern: None,
                    description: None,
                },
                UrlAllowlistEntry {
                    domain: "api.example.com".into(),
                    url_pattern: None,
                    description: None,
                },
            ],
        };
        let rules = allowlist_from_config(&cfg);
        assert_eq!(rules.len(), 1, "invalid entry must be dropped");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p zeroclaw-runtime --lib security::leak_detector::tests::mask_ security::leak_detector::tests::scan_with_masked security::leak_detector::tests::allowlist_from_config_
```

Expected: 3 FAIL with `cannot find function` / `cannot find associated function from_config`.

- [ ] **Step 3: Implement mask/restore + from_config + allowlist_from_config**

In `leak_detector.rs`, add after the `AllowlistRule` block from Task 2:

```rust
const PLACEHOLDER_PREFIX: &str = "\u{00AB}URL_ALLOWLIST_PLACEHOLDER_";
const PLACEHOLDER_SUFFIX: &str = "\u{00BB}";

static URL_FINDER_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    // Greedy URL scan. Stops at whitespace, quotes, angle brackets, or backticks.
    Regex::new(r#"https?://[^\s"'<>`]+"#).unwrap()
});

/// Replace every URL matching any rule with a placeholder, returning the
/// rewritten text plus a mapping the caller passes back to
/// `restore_allowlist_urls`. Non-matching URLs are left untouched.
pub fn mask_allowlist_urls(
    input: &str,
    rules: &[AllowlistRule],
) -> (String, Vec<(String, String)>) {
    if rules.is_empty() {
        return (input.to_string(), Vec::new());
    }
    let mut mapping = Vec::new();
    let mut idx: usize = 0;
    let out = URL_FINDER_RE
        .replace_all(input, |caps: &regex::Captures| {
            let url = caps.get(0).unwrap().as_str();
            if url_matches_any_rule(url, rules) {
                let ph = format!("{PLACEHOLDER_PREFIX}{idx}{PLACEHOLDER_SUFFIX}");
                mapping.push((ph.clone(), url.to_string()));
                idx += 1;
                ph
            } else {
                url.to_string()
            }
        })
        .into_owned();
    (out, mapping)
}

/// Inverse of `mask_allowlist_urls`. Cheap literal `replace` per pair —
/// placeholders are guaranteed unique by construction.
pub fn restore_allowlist_urls(masked: &str, mapping: &[(String, String)]) -> String {
    if mapping.is_empty() {
        return masked.to_string();
    }
    let mut out = masked.to_string();
    for (ph, original) in mapping {
        out = out.replace(ph, original);
    }
    out
}

/// Compile every `UrlAllowlistEntry` in `cfg` to an `AllowlistRule`. Rows
/// that fail to compile are dropped after a single `warn!` so a bad config
/// row doesn't disable the rest.
pub fn allowlist_from_config(
    cfg: &zeroclaw_config::schema::LeakDetectorConfig,
) -> Vec<AllowlistRule> {
    cfg.url_allowlist
        .iter()
        .filter_map(|e| {
            AllowlistRule::new(&e.domain, e.url_pattern.as_deref()).or_else(|| {
                tracing::warn!(
                    domain = %e.domain,
                    url_pattern = ?e.url_pattern,
                    "leak_detector: dropping invalid allowlist entry"
                );
                None
            })
        })
        .collect()
}
```

Then add to the existing `impl LeakDetector` block:

```rust
    pub fn from_config(cfg: &zeroclaw_config::schema::LeakDetectorConfig) -> Self {
        // Same logical shape as `new()` but uses configured sensitivity.
        // The detector does NOT own the allowlist — callers mask first.
        Self {
            sensitivity: cfg.sensitivity,
        }
    }
```

(If `new()`'s body initializes additional fields beyond `sensitivity`, copy that initialization here too — read `new()` first to confirm.)

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p zeroclaw-runtime --lib security::leak_detector
```

Expected: all leak_detector tests PASS (existing + 3 new).

- [ ] **Step 5: Re-export from `security/mod.rs`**

Find the existing `pub use leak_detector::{LeakDetector, LeakResult};` line. Replace with:

```rust
pub use leak_detector::{
    AllowlistRule, LeakDetector, LeakResult, allowlist_from_config, mask_allowlist_urls,
    restore_allowlist_urls, url_matches_any_rule,
};
```

- [ ] **Step 6: Crate compile check**

```bash
cargo check -p zeroclaw-runtime --all-targets
```

Expected: 0 errors.

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-runtime/src/security/leak_detector.rs crates/zeroclaw-runtime/src/security/mod.rs
git commit -m "feat(security): mask_allowlist_urls + restore_allowlist_urls + LeakDetector::from_config (PR #48)"
```

---

## Task 4: `scrub_credentials_with_allowlist`

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/turn/redact.rs`
- Test: same file's existing `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `mask_allowlist_urls`, `restore_allowlist_urls`, `AllowlistRule` (Task 3)
- Produces:
  - `pub fn scrub_credentials_with_allowlist(input: &str, rules: &[AllowlistRule]) -> String`
  - **Contract:** With empty `rules`, output is byte-identical to `scrub_credentials(input)`. This is verified by a test below.

- [ ] **Step 1: Write the failing tests**

Append to the existing test mod at the bottom of `redact.rs`:

```rust
    use crate::security::{AllowlistRule, scrub_credentials_with_allowlist};

    #[test]
    fn scrub_with_allowlist_preserves_token_in_allowlisted_url() {
        let rules = vec![AllowlistRule::new("api.example.com", None).unwrap()];
        let input = "Pay here: https://api.example.com/o?token=hgnD0jgCF63abcdefghij ok";
        let out = scrub_credentials_with_allowlist(input, &rules);
        assert!(
            out.contains("token=hgnD0jgCF63abcdefghij"),
            "allowlisted URL token must survive: {out}"
        );
    }

    #[test]
    fn scrub_with_allowlist_still_redacts_non_allowlisted_tokens() {
        let rules = vec![AllowlistRule::new("api.example.com", None).unwrap()];
        let input = "Authorization: bearer abcdefghijklmnop and url https://evil.com/?token=secret123456";
        let out = scrub_credentials_with_allowlist(input, &rules);
        assert!(!out.contains("abcdefghijklmnop"), "bearer token must be scrubbed: {out}");
        assert!(!out.contains("secret123456"), "evil.com token must be scrubbed: {out}");
    }

    #[test]
    fn scrub_with_empty_allowlist_matches_plain_scrub_credentials() {
        let input = "token=hgnD0jgCF63 and password=\"plaintext123\"";
        assert_eq!(
            scrub_credentials_with_allowlist(input, &[]),
            scrub_credentials(input),
            "empty allowlist must be a strict no-op vs plain scrub_credentials"
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p zeroclaw-runtime --lib agent::turn::redact::tests::scrub_with
```

Expected: 3 FAIL with `cannot find function scrub_credentials_with_allowlist`.

- [ ] **Step 3: Implement**

Add to `redact.rs` after `scrub_credentials`:

```rust
use crate::security::{AllowlistRule, mask_allowlist_urls, restore_allowlist_urls};

/// Allowlist-aware variant of `scrub_credentials`. Empty `rules` is a no-op
/// (byte-identical to `scrub_credentials`) — see test
/// `scrub_with_empty_allowlist_matches_plain_scrub_credentials`.
pub fn scrub_credentials_with_allowlist(input: &str, rules: &[AllowlistRule]) -> String {
    if rules.is_empty() {
        return scrub_credentials(input);
    }
    let (masked, mapping) = mask_allowlist_urls(input, rules);
    let scrubbed = scrub_credentials(&masked);
    restore_allowlist_urls(&scrubbed, &mapping)
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test -p zeroclaw-runtime --lib agent::turn::redact
```

Expected: 3 new PASS + all existing redact tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/zeroclaw-runtime/src/agent/turn/redact.rs
git commit -m "feat(security): scrub_credentials_with_allowlist (PR #48)"
```

---

## Task 5: `scrub_context` task-local module

**Files:**
- Create: `crates/zeroclaw-runtime/src/agent/scrub_context.rs`
- Modify: `crates/zeroclaw-runtime/src/agent/mod.rs` (add `pub mod scrub_context;`)
- Test: in the new file

**Interfaces:**
- Consumes: `AllowlistRule` (Task 3)
- Produces:
  - `pub static TOOL_LOOP_ALLOWLIST: tokio::task_local!<Option<Arc<Vec<AllowlistRule>>>>` (referenced via the `scope` method directly per the task_local macro contract)
  - `pub fn current_allowlist() -> Vec<AllowlistRule>` — clones the inner `Vec` if scope is set, else empty
  - **Pattern reference:** `crates/zeroclaw-runtime/src/agent/cost.rs:86` and `tool_receipts.rs:133` use the same shape.

- [ ] **Step 1: Find `agent/mod.rs` and add the module declaration**

Open `crates/zeroclaw-runtime/src/agent/mod.rs`. Add (alphabetical) `pub mod scrub_context;`.

- [ ] **Step 2: Write the failing test**

Create `crates/zeroclaw-runtime/src/agent/scrub_context.rs` with this content:

```rust
//! Task-local URL allowlist for `scrub_credentials_with_allowlist` callers.

use std::sync::Arc;

use crate::security::AllowlistRule;

tokio::task_local! {
    pub static TOOL_LOOP_ALLOWLIST: Option<Arc<Vec<AllowlistRule>>>;
}

/// Snapshot of the current task-local allowlist. Returns an empty vec when
/// the scope is unset, so callers can pass `&[]` semantics without
/// branching on the option.
pub fn current_allowlist() -> Vec<AllowlistRule> {
    TOOL_LOOP_ALLOWLIST
        .try_with(|slot| {
            slot.as_ref()
                .map(|arc| arc.as_ref().clone())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn current_allowlist_returns_empty_when_unset() {
        assert!(current_allowlist().is_empty());
    }

    #[tokio::test]
    async fn current_allowlist_returns_rules_inside_scope() {
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let arc = Arc::new(vec![rule]);
        TOOL_LOOP_ALLOWLIST
            .scope(Some(arc.clone()), async {
                let got = current_allowlist();
                assert_eq!(got.len(), 1);
            })
            .await;
    }

    #[tokio::test]
    async fn current_allowlist_returns_empty_when_scope_value_is_none() {
        TOOL_LOOP_ALLOWLIST
            .scope(None, async {
                assert!(current_allowlist().is_empty());
            })
            .await;
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

```bash
cargo test -p zeroclaw-runtime --lib agent::scrub_context
```

Expected: 3 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/zeroclaw-runtime/src/agent/scrub_context.rs crates/zeroclaw-runtime/src/agent/mod.rs
git commit -m "feat(security): TOOL_LOOP_ALLOWLIST task-local for allowlist propagation (PR #48)"
```

---

## Task 6: Replace `scrub_credentials` call sites with allowlist-aware variant

**Files:**
- Modify: `crates/zeroclaw-runtime/src/agent/loop_.rs:422`
- Modify: `crates/zeroclaw-runtime/src/agent/tool_execution.rs:100,199,223,261`
- Test: an integration test inside `tool_execution.rs` (or a new tests submodule near the changes) that exercises the task-local fall-through.

**Interfaces:**
- Consumes: `scrub_credentials_with_allowlist` (Task 4), `current_allowlist` (Task 5)
- Produces: behavioral change only — call sites read the task-local allowlist instead of unconditionally calling `scrub_credentials`.

- [ ] **Step 1: Audit call sites**

Run the audit to confirm the exact 5 sites:

```bash
grep -n "scrub_credentials\b" crates/zeroclaw-runtime/src/agent/loop_.rs crates/zeroclaw-runtime/src/agent/tool_execution.rs
```

Expected output (line numbers may drift if Task 1–5 changed adjacent code; the count must be exactly 5 call sites, plus definitions/test helpers which you leave alone). If the count differs, **stop and investigate** before editing — a prior task may have moved code.

- [ ] **Step 2: Write the failing integration test**

Append to the existing test module in `tool_execution.rs` (find the `#[cfg(test)] mod tests` already present; if none, create one at file bottom):

```rust
#[cfg(test)]
mod allowlist_integration_tests {
    use super::*;
    use std::sync::Arc;
    use crate::agent::scrub_context::TOOL_LOOP_ALLOWLIST;
    use crate::security::AllowlistRule;

    // Smallest helper that hits `scrub_credentials` indirectly. Pick the
    // public function on `tool_execution.rs` that wraps a `scrub_credentials`
    // call and returns the scrubbed string. If no such function exists,
    // expose a `pub(crate)` shim named `scrub_for_tool_output` around the
    // call site so this test can drive it directly.
    #[tokio::test]
    async fn allowlisted_url_token_survives_tool_output_scrub() {
        let raw = "QR: https://api.example.com/o?token=hgnD0jgCF63abcdefghij done";
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let scrubbed = TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async move {
                // Call the wrapper that internally does `scrub_credentials_with_allowlist(..., &current_allowlist())`.
                scrub_for_tool_output(raw)
            })
            .await;
        assert!(
            scrubbed.contains("token=hgnD0jgCF63abcdefghij"),
            "allowlisted token must survive: {scrubbed}"
        );
    }

    #[tokio::test]
    async fn non_allowlisted_url_token_still_scrubbed() {
        let raw = "evil: https://evil.com/x?token=abcdefghijklmnop done";
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let scrubbed = TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async move { scrub_for_tool_output(raw) })
            .await;
        assert!(!scrubbed.contains("abcdefghijklmnop"), "got: {scrubbed}");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

```bash
cargo test -p zeroclaw-runtime --lib agent::tool_execution::allowlist_integration_tests
```

Expected: FAIL with `cannot find function scrub_for_tool_output`.

- [ ] **Step 4: Refactor call sites**

In `tool_execution.rs`, replace each `scrub_credentials(x)` (at lines 100, 199, 223, 261 — re-locate with grep if drifted) with `scrub_credentials_with_allowlist(x, &crate::agent::scrub_context::current_allowlist())`. Update the `use` at top:

```rust
use super::loop_::{ParsedToolCall, ToolLoopCancelled, is_tool_loop_cancelled};
use super::turn::redact::{scrub_credentials, scrub_credentials_with_allowlist};
use crate::agent::scrub_context::current_allowlist;
```

Then add a small wrapper near the top of `tool_execution.rs` to keep tests independent of refactors:

```rust
pub(crate) fn scrub_for_tool_output(raw: &str) -> String {
    scrub_credentials_with_allowlist(raw, &current_allowlist())
}
```

Update each of the 4 sites to call `scrub_for_tool_output(x)` instead of `scrub_credentials(x)`. The variable names (`reason`, `output`, etc.) stay identical.

In `loop_.rs:422`, change:

```rust
Some(truncate_with_ellipsis(&scrub_credentials(raw), 200))
```

to:

```rust
Some(truncate_with_ellipsis(
    &scrub_credentials_with_allowlist(raw, &crate::agent::scrub_context::current_allowlist()),
    200,
))
```

Update the `use` block at the top of `loop_.rs` only if `scrub_credentials_with_allowlist` is not already in scope (it should resolve via `agent::turn::redact::scrub_credentials_with_allowlist` — add an import if needed).

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p zeroclaw-runtime --lib agent::tool_execution::allowlist_integration_tests
```

Expected: 2 PASS.

- [ ] **Step 6: Run wider test sweep to catch regressions**

```bash
cargo test -p zeroclaw-runtime --lib agent::
```

Expected: 0 new failures.

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-runtime/src/agent/tool_execution.rs crates/zeroclaw-runtime/src/agent/loop_.rs
git commit -m "refactor(security): route scrub_credentials call sites through TOOL_LOOP_ALLOWLIST (PR #48)"
```

---

## Task 7: Orchestrator — switch `LeakDetector::new()` → `from_config` and install the scope

**Files:**
- Modify: `crates/zeroclaw-channels/src/orchestrator/mod.rs:{3438,3468,9831}` (signature + 2 callsites + scope installer)
- Test: append to existing test mod in `orchestrator/mod.rs` (the file already has extensive `sanitize_channel_response_*` tests around lines 11477+ and 20690)

**Interfaces:**
- Consumes: `LeakDetector::from_config`, `allowlist_from_config` (Task 3), `TOOL_LOOP_ALLOWLIST` (Task 5)
- Produces: behavioral wiring — `sanitize_channel_response` gains a `leak_detector_config: &LeakDetectorConfig` parameter; both `LeakDetector::new()` call sites switch to `from_config`; the message-processing entry point wraps work in `TOOL_LOOP_ALLOWLIST::scope`.

- [ ] **Step 1: Confirm call sites and signature**

```bash
grep -n "LeakDetector::new\|sanitize_channel_response\b\|fn process_channel_message\|fn deliver_response\b" crates/zeroclaw-channels/src/orchestrator/mod.rs | head -20
```

Note the exact line numbers (they may have shifted by ±20 from this plan). The signature today is:

```rust
fn sanitize_channel_response(response: &str, tools: &[Box<dyn Tool>]) -> String
```

with `LeakDetector::new().scan(&sanitized)` inside it (line 3468), and a second `LeakDetector::new()` near line 9831 inside the deliver path.

- [ ] **Step 2: Write the failing test**

Append to the test mod in `orchestrator/mod.rs` (anywhere near line 20690 where similar tests live):

```rust
    #[test]
    fn sanitize_channel_response_preserves_allowlisted_url_token() {
        use zeroclaw_config::schema::{LeakDetectorConfig, UrlAllowlistEntry};
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let input = "QR: https://api.example.com/o?token=hgnD0jgCF63abcdefghij done";
        let cfg = LeakDetectorConfig {
            sensitivity: 0.7,
            url_allowlist: vec![UrlAllowlistEntry {
                domain: "api.example.com".into(),
                url_pattern: None,
                description: None,
            }],
        };
        let out = sanitize_channel_response(input, &tools, &cfg);
        assert!(
            out.contains("token=hgnD0jgCF63abcdefghij"),
            "allowlisted URL token must survive: {out}"
        );
    }
```

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo test -p zeroclaw-channels --lib sanitize_channel_response_preserves_allowlisted
```

Expected: FAIL with arity mismatch (`expected 2 arguments, found 3`).

- [ ] **Step 4: Update the signature + call sites**

In `orchestrator/mod.rs`:

a) Change `sanitize_channel_response`'s signature at line ~3438:

```rust
fn sanitize_channel_response(
    response: &str,
    tools: &[Box<dyn Tool>],
    leak_detector_config: &zeroclaw_config::schema::LeakDetectorConfig,
) -> String {
```

b) Replace the body's leak scan (line ~3468):

```rust
let rules = zeroclaw_runtime::security::allowlist_from_config(leak_detector_config);
let (masked, mapping) = zeroclaw_runtime::security::mask_allowlist_urls(&sanitized, &rules);
let detector = zeroclaw_runtime::security::LeakDetector::from_config(leak_detector_config);
match detector.scan(&masked) {
    zeroclaw_runtime::security::LeakResult::Clean => {
        zeroclaw_runtime::security::restore_allowlist_urls(&masked, &mapping)
    }
    zeroclaw_runtime::security::LeakResult::Detected { patterns, redacted } => {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
        );
        let _ = patterns; // existing logging fields preserved — keep original lines
        zeroclaw_runtime::security::restore_allowlist_urls(&redacted, &mapping)
    }
}
```

(Open the original block first and keep all `tracing::warn!` / `Event` fields verbatim — only the surrounding mask/restore is new.)

c) Replace the second `LeakDetector::new()` at line ~9831 similarly. That site is in the `deliver_response`-style path; thread `leak_detector_config` in from the enclosing function — search upward for the nearest `&Config` argument and pass `&config.security.leak_detector` down through any wrapper functions on the way.

d) **Every** call site of `sanitize_channel_response` must now pass `&config.security.leak_detector`. Find them:

```bash
grep -n "sanitize_channel_response(" crates/zeroclaw-channels/src/orchestrator/mod.rs
```

Add the third arg to each call.

e) Install the task-local scope around the per-message work. Locate the function that drives a single inbound channel message (`process_channel_message` or whichever async fn at the channel ingress holds `config: &Config`). Wrap its body:

```rust
let rules = zeroclaw_runtime::security::allowlist_from_config(&config.security.leak_detector);
let scope_value = if rules.is_empty() {
    None
} else {
    Some(std::sync::Arc::new(rules))
};
zeroclaw_runtime::agent::scrub_context::TOOL_LOOP_ALLOWLIST
    .scope(scope_value, async move {
        // original body
    })
    .await
```

If the original body returns `Result<T>` / `()` etc., propagate that through the `.scope(...).await` — the `scope` future has the same output type as its inner future.

- [ ] **Step 5: Run test to verify it passes**

```bash
cargo test -p zeroclaw-channels --lib sanitize_channel_response_preserves_allowlisted
```

Expected: PASS.

- [ ] **Step 6: Full workspace check**

```bash
cargo check --workspace --all-targets
cargo test -p zeroclaw-channels --lib
cargo test -p zeroclaw-runtime --lib
```

Expected: 0 errors / 0 new test failures.

- [ ] **Step 7: Commit**

```bash
git add crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(security): wire LeakDetectorConfig + TOOL_LOOP_ALLOWLIST through orchestrator (PR #48)"
```

---

## Task 8: Migration tracking doc — flip #48 to ✅

**Files:**
- Modify: `docs/maintainers/migration-tracking-TBD.md`

- [ ] **Step 1: Update the #48 row**

Open `docs/maintainers/migration-tracking-TBD.md`. Find the `**#48**` row in the migration table. Replace its `状态` and `最终结论` columns:

| was | becomes |
|-----|---------|
| `❌ 未迁移` | `✅ 已迁移` |
| 最终结论列原文 | `迁移完成。适配 0.8.0 现状: scrub_credentials 调用点 5 处 (vs master 1 处) 全部改造; 引入 \`crates/zeroclaw-runtime/src/agent/scrub_context.rs\` 承载 TOOL_LOOP_ALLOWLIST; sanitize_channel_response 加 \`&LeakDetectorConfig\` 参数。配置入口 \`[security.leak_detector]\`,默认空白名单,行为完全向后兼容。` |

Adjust the "推荐执行顺序" P2 bullet: drop `/#48`, keep #41 alone (or note "#48 已迁移").

- [ ] **Step 2: Commit**

```bash
git add docs/maintainers/migration-tracking-TBD.md
git commit -m "docs(migration): mark PR #48 (URL allowlist) as migrated"
```

---

## Self-Review Notes

- **Spec coverage:** All four spec deliverables — `LeakDetectorConfig` schema, mask/restore pair, `scrub_credentials_with_allowlist`, end-to-end propagation via task-local — are owned by Tasks 1, 3, 4, 5+6+7 respectively. The mask-then-detect-then-restore architectural commitment is enforced by Task 3 Step 1's `scan_with_masked_allowlist_preserves_token` test.
- **Placeholder scan:** No `TBD` / `implement later` / "similar to Task N" wording. Every code snippet is complete and runnable.
- **Type consistency:** `AllowlistRule` introduced in Task 2, consumed in Tasks 3/4/5/6/7 with identical signature. `LeakDetectorConfig` introduced in Task 1, consumed in Tasks 3/7. `TOOL_LOOP_ALLOWLIST` introduced in Task 5, consumed in Tasks 6/7.
- **0.8.0 vs master deltas explicitly handled:**
  - `scrub_credentials` lives in `agent/turn/redact.rs` (Task 4) — master puts it in `loop_.rs`
  - 5 scrub call sites (Task 6) — master has 1
  - 2 `LeakDetector::new()` call sites (Task 7) — master refactor signature, both sites updated
- **Risk:** Lowest in Task 1 (additive schema), highest in Task 7 (signature change + scope plumbing). Each task ends with a green workspace, so a stuck Task 7 still leaves Tasks 1–6 mergeable.
