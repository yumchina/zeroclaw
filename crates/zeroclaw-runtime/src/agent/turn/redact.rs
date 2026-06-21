//! Credential redaction for the rendering layer (logs, observer events, and
//! UI-facing turn events). This never runs on the data path: tool results fed
//! back to the model and signed by HMAC receipts always carry raw bytes.

use regex::Regex;
use std::sync::LazyLock;
use crate::security::{AllowlistRule, mask_allowlist_urls, restore_allowlist_urls};

static SENSITIVE_KV_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(token|api[_-]?key|password|secret|user[_-]?key|bearer|credential)["']?\s*[:=]\s*(?:"([^"]{8,})"|'([^']{8,})'|([a-zA-Z0-9_\-\./+=]{8,}))"#).unwrap()
});

/// Scrub credentials from text bound for a human-facing surface (log records,
/// observer event fields, UI/editor turn events). Replaces known credential
/// patterns with a redacted placeholder while preserving a small prefix for
/// context. Callers must apply this only at the rendering boundary, never to
/// the output that flows back into the agent loop.
pub fn scrub_credentials(input: &str) -> String {
    SENSITIVE_KV_REGEX
        .replace_all(input, |caps: &regex::Captures| {
            let full_match = &caps[0];
            let key = &caps[1];
            let val = caps
                .get(2)
                .or(caps.get(3))
                .or(caps.get(4))
                .map(|m| m.as_str())
                .unwrap_or("");

            // Preserve first 4 chars for context, then redact.
            // Use char_indices to find the byte offset of the 4th character
            // so we never slice in the middle of a multi-byte UTF-8 sequence.
            let prefix = if val.len() > 4 {
                val.char_indices()
                    .nth(4)
                    .map(|(byte_idx, _)| &val[..byte_idx])
                    .unwrap_or(val)
            } else {
                ""
            };

            if full_match.contains(':') {
                if full_match.contains('"') {
                    format!("\"{}\": \"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}: {}*[REDACTED]", key, prefix)
                }
            } else if full_match.contains('=') {
                if full_match.contains('"') {
                    format!("{}=\"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}={}*[REDACTED]", key, prefix)
                }
            } else {
                format!("{}: {}*[REDACTED]", key, prefix)
            }
        })
        .to_string()
}

/// Allowlist-aware variant of `scrub_credentials`. Pre-masks allowlisted URLs
/// before redaction, then restores them after. Empty `rules` short-circuits
/// to plain `scrub_credentials` for byte-identical semantics — see test
/// `scrub_with_empty_allowlist_matches_plain_scrub_credentials`.
pub fn scrub_credentials_with_allowlist(input: &str, rules: &[AllowlistRule]) -> String {
    if rules.is_empty() {
        return scrub_credentials(input);
    }
    let (masked, mapping) = mask_allowlist_urls(input, rules);
    let scrubbed = scrub_credentials(&masked);
    restore_allowlist_urls(&scrubbed, &mapping)
}

#[cfg(test)]
mod tests {
    use super::{scrub_credentials, scrub_credentials_with_allowlist};
    use crate::security::AllowlistRule;

    #[test]
    fn scrub_credentials_redacts_unquoted_base64_credential_values() {
        let input = "token=QWxh+GRpbjpvcGVu/IHNlc2FtZQ== next=public";
        let scrubbed = scrub_credentials(input);

        assert_eq!(scrubbed, "token=QWxh*[REDACTED] next=public");
        assert!(!scrubbed.contains("IHNlc2FtZQ"));
        assert!(!scrubbed.contains("=="));
    }

    #[test]
    fn scrub_credentials_redacts_quoted_base64_credential_values() {
        let input = r#"secret="QWxhZGRpbjpvcGVu/IHNlc2FtZQ==""#;
        let scrubbed = scrub_credentials(input);

        assert_eq!(scrubbed, r#"secret="QWxh*[REDACTED]""#);
        assert!(!scrubbed.contains("IHNlc2FtZQ"));
        assert!(!scrubbed.contains("=="));
    }

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
        let input = "api_key=abcdefghijklmnop and url https://evil.com/?token=secret123456";
        let out = scrub_credentials_with_allowlist(input, &rules);
        assert!(!out.contains("abcdefghijklmnop"), "api_key token must be scrubbed: {out}");
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

    /// End-to-end: when the orchestrator sets TOOL_LOOP_ALLOWLIST and a
    /// renderer calls `scrub_credentials_with_allowlist(x, &current_allowlist())`,
    /// allowlisted-host URL tokens survive. This is the post-merge home of
    /// PR #48's tool-execution-layer integration test.
    #[tokio::test]
    async fn allowlisted_url_token_survives_rendering_scrub() {
        use crate::agent::scrub_context::{TOOL_LOOP_ALLOWLIST, current_allowlist};
        use std::sync::Arc;

        let raw = "QR: https://api.example.com/o?token=hgnD0jgCF63abcdefghij done";
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let scrubbed = TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async move {
                scrub_credentials_with_allowlist(raw, &current_allowlist())
            })
            .await;
        assert!(
            scrubbed.contains("token=hgnD0jgCF63abcdefghij"),
            "allowlisted token must survive: {scrubbed}"
        );
    }

    #[tokio::test]
    async fn non_allowlisted_url_token_still_scrubbed_in_rendering() {
        use crate::agent::scrub_context::{TOOL_LOOP_ALLOWLIST, current_allowlist};
        use std::sync::Arc;

        let raw = "evil: https://evil.com/x?token=abcdefghijklmnop done";
        let rule = AllowlistRule::new("api.example.com", None).unwrap();
        let scope_value = Some(Arc::new(vec![rule]));
        let scrubbed = TOOL_LOOP_ALLOWLIST
            .scope(scope_value, async move {
                scrub_credentials_with_allowlist(raw, &current_allowlist())
            })
            .await;
        assert!(!scrubbed.contains("abcdefghijklmnop"), "got: {scrubbed}");
    }
}
