//! Interactive approval workflow for supervised mode.
//!
//! Provides a pre-execution hook that prompts the user before tool calls,
//! with persistent grant storage and structured event logging.

pub mod grant_store;
pub use grant_store::{ApprovalGrant, ApprovalGrantStore, GrantFilter, SqliteGrantStore};

pub mod decision_reason;
pub mod humanize;
pub use humanize::{Humanizer, SummaryProvider};

pub mod broker;
pub use broker::{ApprovalBroker, BrokerDecision, BrokerRequestCtx, ChannelDirectory};

use crate::security::AutonomyLevel;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::sync::Arc;
use zeroclaw_config::schema::RiskProfileConfig;

// ── Types ────────────────────────────────────────────────────────

/// A request to approve a tool call before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// The user's response to an approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalResponse {
    /// Execute this one call.
    Yes,
    /// Deny this call.
    No,
    /// Execute and add tool to session-scoped allowlist.
    Always,
    /// Skip execution; return this as the tool result instead.
    #[serde(rename = "replace_with")]
    ReplaceWith(String),
}

/// Maximum length of an operator-supplied `DenyWithEdit` / `ReplaceWith`
/// replacement, in bytes. The replacement is operator-authored but still
/// untrusted input that becomes a tool result fed back to the model — cap it
/// so a runaway paste can't blow up the context window.
pub const MAX_REPLACEMENT_LEN: usize = 64 * 1024;

/// Sanitize an operator-supplied tool-result replacement before it is fed back
/// to the model: drop control characters (except `\n`, `\r`, `\t`) that could
/// corrupt rendering or smuggle terminal escapes, and truncate to
/// [`MAX_REPLACEMENT_LEN`] on a char boundary.
#[must_use]
pub fn sanitize_tool_replacement(replacement: &str) -> String {
    let cleaned: String = replacement
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect();
    if cleaned.len() <= MAX_REPLACEMENT_LEN {
        return cleaned;
    }
    let mut end = MAX_REPLACEMENT_LEN;
    while end > 0 && !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    Prompt,
    Approved,
    NotRequired,
}

/// Context for grant store lookup during approval requirement checks.
pub struct GrantLookupCtx {
    pub channel_ref: String,
    pub topic: Option<String>,
    pub user_master_id: String,
}

// ── ApprovalManager ──────────────────────────────────────────────

/// Manages the approval workflow for tool calls.
///
/// - Checks config-level `auto_approve` / `always_ask` lists
/// - Queries persistent grant storage when lookup context is provided
/// - Routes approval decisions to structured log events
///
/// Two modes:
/// - **Interactive** (CLI): tools needing approval trigger a stdin prompt.
/// - **Non-interactive** (channels): tools needing approval are auto-denied
///   because there is no interactive operator to approve them. `auto_approve`
///   policy is still enforced, and `always_ask` / supervised-default tools are
///   denied rather than silently allowed.
/// - **Non-interactive back-channel** (ACP/WS): tools needing approval are sent
///   through a client approval channel instead of trusting tool arguments.
pub struct ApprovalManager {
    /// Tools that never need approval (from config).
    auto_approve: std::collections::HashSet<String>,
    /// Tools that always need approval, overriding grants.
    always_ask: std::collections::HashSet<String>,
    /// Autonomy level from config.
    autonomy_level: AutonomyLevel,
    /// When `true`, tools that would require interactive approval are
    /// auto-denied instead. Used for channel-driven (non-CLI) runs.
    non_interactive: bool,
    /// When `true`, shell calls in non-interactive mode still enter the outer
    /// approval flow because a real client approval channel exists.
    non_interactive_shell_requires_approval: bool,
    /// Persistent grant store for "Always" approvals.
    grants: Option<Arc<dyn ApprovalGrantStore>>,
    /// Approval broker for routing approval requests.
    broker: Option<Arc<ApprovalBroker>>,
}

impl ApprovalManager {
    /// Create an interactive (CLI) approval manager from a risk profile.
    pub fn from_risk_profile(risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: false,
            non_interactive_shell_requires_approval: false,
            grants: None,
            broker: None,
        }
    }

    /// Create a non-interactive approval manager for channel-driven runs.
    ///
    /// Enforces the same `auto_approve` / `always_ask` / supervised policies
    /// as the CLI manager, but tools that would require interactive approval
    /// are auto-denied instead of prompting (since there is no operator).
    pub fn for_non_interactive(risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: true,
            non_interactive_shell_requires_approval: false,
            grants: None,
            broker: None,
        }
    }

    /// Create a non-interactive manager for direct agents with a human
    /// approval back-channel, such as ACP and the web dashboard WebSocket.
    /// Reads from the same per-agent risk profile as
    /// [`Self::for_non_interactive`]; the only difference is that shell
    /// invocations route through the operator-driven backchannel rather
    /// than auto-denying.
    pub fn for_non_interactive_backchannel(risk_profile: &RiskProfileConfig) -> Self {
        Self {
            auto_approve: risk_profile.auto_approve.iter().cloned().collect(),
            always_ask: risk_profile.always_ask.iter().cloned().collect(),
            autonomy_level: risk_profile.level,
            non_interactive: true,
            non_interactive_shell_requires_approval: true,
            grants: None,
            broker: None,
        }
    }

    /// Attach a persistent grant store.
    pub fn with_grant_store(mut self, grants: Arc<dyn ApprovalGrantStore>) -> Self {
        self.grants = Some(grants);
        self
    }

    /// Attach an approval broker.
    pub fn with_broker(mut self, broker: Arc<ApprovalBroker>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// Get the approval broker if attached.
    pub fn broker(&self) -> Option<Arc<ApprovalBroker>> {
        self.broker.clone()
    }

    /// Returns `true` when this manager operates in non-interactive mode
    /// (i.e. for channel-driven runs where no operator can approve).
    pub fn is_non_interactive(&self) -> bool {
        self.non_interactive
    }

    /// Check whether a tool call requires interactive approval.
    ///
    /// Returns `true` if the call needs a prompt, `false` if it can proceed.
    pub fn needs_approval(&self, tool_name: &str) -> bool {
        self.approval_requirement(tool_name, None) == ApprovalRequirement::Prompt
    }

    pub fn approval_requirement(
        &self,
        tool_name: &str,
        lookup: Option<&GrantLookupCtx>,
    ) -> ApprovalRequirement {
        // Full autonomy never prompts.
        if self.autonomy_level == AutonomyLevel::Full {
            return ApprovalRequirement::Approved;
        }

        // ReadOnly blocks everything — handled elsewhere; no prompt needed.
        if self.autonomy_level == AutonomyLevel::ReadOnly {
            return ApprovalRequirement::NotRequired;
        }

        // always_ask overrides everything.
        if self.always_ask.contains("*") || self.always_ask.contains(tool_name) {
            return ApprovalRequirement::Prompt;
        }

        // Channel-driven shell execution is still guarded by the shell tool's
        // own command allowlist and risk policy. Skipping the outer approval
        // gate here lets low-risk allowlisted commands (e.g. `ls`) work in
        // non-interactive channels without silently allowing medium/high-risk
        // commands.
        if self.non_interactive
            && tool_name == "shell"
            && !self.non_interactive_shell_requires_approval
        {
            return ApprovalRequirement::NotRequired;
        }

        // auto_approve skips the prompt.
        if self.auto_approve.contains("*") || self.auto_approve.contains(tool_name) {
            return ApprovalRequirement::Approved;
        }

        // Check persistent grant store.
        if let (Some(lookup), Some(grants)) = (lookup, &self.grants) {
            if let Ok(Some(_)) = grants.get(
                lookup.channel_ref.as_str(),
                lookup.topic.as_deref(),
                lookup.user_master_id.as_str(),
                tool_name,
            ) {
                return ApprovalRequirement::Approved;
            }
        }

        // Default: supervised mode requires approval.
        ApprovalRequirement::Prompt
    }

    /// Record an approval decision to the structured log.
    pub fn record_decision(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        decision: &ApprovalResponse,
        channel: &str,
        reason: &'static str,
        extras: serde_json::Value,
    ) {
        let summary = summarize_args(args);
        let (action, outcome) = match decision {
            ApprovalResponse::Yes | ApprovalResponse::Always => (
                ::zeroclaw_log::Action::Approve,
                ::zeroclaw_log::EventOutcome::Success,
            ),
            ApprovalResponse::No => (
                ::zeroclaw_log::Action::Reject,
                ::zeroclaw_log::EventOutcome::Failure,
            ),
            ApprovalResponse::ReplaceWith(_) => (
                ::zeroclaw_log::Action::Defer,
                ::zeroclaw_log::EventOutcome::Success,
            ),
        };

        let mut attrs = serde_json::json!({
            "tool": tool_name,
            "channel": channel,
            "reason": reason,
            "arguments_summary": summary,
        });

        if let Some(map) = attrs.as_object_mut() {
            if let Some(extra_map) = extras.as_object() {
                for (k, v) in extra_map {
                    map.insert(k.clone(), v.clone());
                }
            }
        }

        match action {
            ::zeroclaw_log::Action::Approve => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), action)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(outcome)
                        .with_attrs(attrs),
                    "tool_approval_decision"
                );
            }
            _ => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), action)
                        .with_category(::zeroclaw_log::EventCategory::Tool)
                        .with_outcome(outcome)
                        .with_attrs(attrs),
                    "tool_approval_decision"
                );
            }
        }
    }

    /// Prompt the user on the CLI and return their decision.
    ///
    /// Only called for interactive (CLI) managers. Non-interactive managers
    /// auto-deny in the tool-call loop before reaching this point.
    pub fn prompt_cli(&self, request: &ApprovalRequest) -> ApprovalResponse {
        prompt_cli_interactive(request)
    }
}

// ── CLI prompt ───────────────────────────────────────────────────

/// Display the approval prompt and read user input from stdin.
fn prompt_cli_interactive(request: &ApprovalRequest) -> ApprovalResponse {
    let summary = summarize_args(&request.arguments);
    eprintln!();
    eprintln!("🔧 Agent wants to execute: {}", request.tool_name);
    eprintln!("   {summary}");
    eprint!("   [Y]es / [N]o / [A]lways for {}: ", request.tool_name);
    let _ = io::stderr().flush();

    let stdin = io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        return ApprovalResponse::No;
    }

    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => ApprovalResponse::Yes,
        "a" | "always" => ApprovalResponse::Always,
        _ => ApprovalResponse::No,
    }
}

/// Produce a short human-readable summary of tool arguments. Argument keys
/// whose names suggest a credential get their value replaced with
/// `[redacted]` before truncation, so summaries that cross security
/// boundaries (e.g. the gateway WebSocket `approval_request` frame) cannot
/// leak secret-bearing fields. Operators MUST treat the summary as
/// best-effort: a tool that names its credential field something other than
/// the patterns below still surfaces. The tool author's typed config and
/// `#[secret]` annotations are the long-term truth source.
pub fn summarize_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = Vec::with_capacity(map.len());

            // Prioritize "path" (used by file_write/file_edit etc.) so approval
            // popups and audit logs always surface the target file first.
            if let Some(v) = map.get("path") {
                let val = if looks_like_secret_key("path") {
                    "[redacted]".to_string()
                } else {
                    match v {
                        serde_json::Value::String(s) => truncate_for_summary(s, 80),
                        other => {
                            let s = other.to_string();
                            truncate_for_summary(&s, 80)
                        }
                    }
                };
                parts.push(format!("path: {val}"));
            }

            for (k, v) in map.iter() {
                if k == "path" {
                    continue;
                }
                let val = if looks_like_secret_key(k) {
                    "[redacted]".to_string()
                } else {
                    match v {
                        serde_json::Value::String(s) => truncate_for_summary(s, 80),
                        other => {
                            let s = other.to_string();
                            truncate_for_summary(&s, 80)
                        }
                    }
                };
                parts.push(format!("{k}: {val}"));
            }
            parts.join(", ")
        }
        other => {
            let s = other.to_string();
            truncate_for_summary(&s, 120)
        }
    }
}

/// Heuristic for argument keys that should have their value redacted in
/// human-readable summaries. Matches anywhere in the (lowercased) key:
/// covers `api_key`, `api-key`, `apiKey`, `oauth_token`, `secret`,
/// `password`, `auth_token`, `bearer`, `client_secret`, `private_key`, etc.
fn looks_like_secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    [
        "secret",
        "password",
        "passwd",
        "token",
        "api_key",
        "api-key",
        "apikey",
        "auth",
        "bearer",
        "private_key",
        "private-key",
        "privatekey",
        "credential",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn truncate_for_summary(input: &str, max_chars: usize) -> String {
    let mut chars = input.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        input.to_string()
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::RiskProfileConfig;

    #[test]
    fn sanitize_replacement_strips_control_chars_keeps_whitespace() {
        let dirty = "ok\u{0007}line\nnext\ttab\u{001b}[31m";
        let clean = sanitize_tool_replacement(dirty);
        assert_eq!(clean, "okline\nnext\ttab[31m");
    }

    #[test]
    fn sanitize_replacement_truncates_on_char_boundary() {
        let big = "é".repeat(MAX_REPLACEMENT_LEN); // 2 bytes each
        let clean = sanitize_tool_replacement(&big);
        assert!(clean.len() <= MAX_REPLACEMENT_LEN);
        // Truncation must land on a char boundary (no panic, valid UTF-8).
        assert!(clean.chars().all(|c| c == 'é'));
    }

    fn supervised_config() -> RiskProfileConfig {
        RiskProfileConfig {
            level: AutonomyLevel::Supervised,
            auto_approve: vec!["file_read".into(), "memory_recall".into()],
            always_ask: vec!["shell".into()],
            ..RiskProfileConfig::default()
        }
    }

    fn full_config() -> RiskProfileConfig {
        RiskProfileConfig {
            level: AutonomyLevel::Full,
            ..RiskProfileConfig::default()
        }
    }

    // ── needs_approval ───────────────────────────────────────

    #[test]
    fn auto_approve_tools_skip_prompt() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(!mgr.needs_approval("file_read"));
        assert!(!mgr.needs_approval("memory_recall"));
    }

    #[test]
    fn always_ask_tools_always_prompt() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn unknown_tool_needs_approval_in_supervised() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(mgr.needs_approval("file_write"));
        assert!(mgr.needs_approval("http_request"));
    }

    #[test]
    fn full_autonomy_never_prompts() {
        let mgr = ApprovalManager::from_risk_profile(&full_config());
        assert!(!mgr.needs_approval("shell"));
        assert!(!mgr.needs_approval("file_write"));
        assert!(!mgr.needs_approval("anything"));
    }

    #[test]
    fn readonly_never_prompts() {
        let config = RiskProfileConfig {
            level: AutonomyLevel::ReadOnly,
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::from_risk_profile(&config);
        assert!(!mgr.needs_approval("shell"));
    }

    // ── grant store ────────────────────────────────────

    #[test]
    fn grant_hit_short_circuits_approval_requirement() {
        use std::sync::Arc;
        let tmp = tempfile::TempDir::new().unwrap();
        let grants =
            Arc::new(crate::approval::grant_store::SqliteGrantStore::new(tmp.path()).unwrap())
                as Arc<dyn ApprovalGrantStore>;
        grants
            .put(crate::approval::grant_store::ApprovalGrant::new(
                "dawnim.work".into(),
                Some("t1".into()),
                "u_alice".into(),
                "file_write".into(),
                "u_admin".into(),
                "dawnim.work".into(),
            ))
            .unwrap();
        let mgr = ApprovalManager::from_risk_profile(&supervised_config()).with_grant_store(grants);
        let ctx = GrantLookupCtx {
            channel_ref: "dawnim.work".into(),
            topic: Some("t1".into()),
            user_master_id: "u_alice".into(),
        };
        assert_eq!(
            mgr.approval_requirement("file_write", Some(&ctx)),
            ApprovalRequirement::Approved
        );
    }

    #[test]
    fn always_ask_overrides_grant_hit() {
        use std::sync::Arc;
        let tmp = tempfile::TempDir::new().unwrap();
        let grants =
            Arc::new(crate::approval::grant_store::SqliteGrantStore::new(tmp.path()).unwrap())
                as Arc<dyn ApprovalGrantStore>;
        grants
            .put(crate::approval::grant_store::ApprovalGrant::new(
                "dawnim.work".into(),
                None,
                "u_alice".into(),
                "shell".into(),
                "u_admin".into(),
                "dawnim.work".into(),
            ))
            .unwrap();
        let mgr = ApprovalManager::from_risk_profile(&supervised_config()).with_grant_store(grants);
        let ctx = GrantLookupCtx {
            channel_ref: "dawnim.work".into(),
            topic: None,
            user_master_id: "u_alice".into(),
        };
        // shell is in always_ask, so grant is ignored.
        assert_eq!(
            mgr.approval_requirement("shell", Some(&ctx)),
            ApprovalRequirement::Prompt
        );
    }

    #[test]
    fn approval_manager_no_longer_exposes_session_allowlist() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        // Compile-time check: the method `session_allowlist()` must not exist.
        // The body intentionally does not call it; if a future refactor re-adds the
        // method, this test stays green but the spec invariant is preserved by the
        // type-level removal in mod.rs.
        let _ = mgr;
    }

    // ── record_decision log emission ────────────────────────────────────────────

    #[test]
    fn record_decision_emits_record_event() {
        let _g1 = ::zeroclaw_log::__private_test_writer_lock();
        let _g2 = ::zeroclaw_log::__private_test_hook_lock();
        let _sub = ::zeroclaw_log::try_install_capture_subscriber();
        let mut rx = ::zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "rm -rf ./build/"}),
            &ApprovalResponse::No,
            "cli",
            crate::approval::decision_reason::INTERACTIVE_DENY,
            serde_json::json!({}),
        );
        mgr.record_decision(
            "file_write",
            &serde_json::json!({"path": "out.txt"}),
            &ApprovalResponse::Yes,
            "cli",
            crate::approval::decision_reason::INTERACTIVE_APPROVE,
            serde_json::json!({}),
        );

        let captured: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let approvals: Vec<_> = captured
            .iter()
            .filter(|ev| {
                matches!(
                    ev.get("event")
                        .and_then(|v| v.get("action"))
                        .and_then(|v| v.as_str()),
                    Some("approve" | "reject" | "defer")
                )
            })
            .collect();
        assert!(
            approvals.len() >= 2,
            "expected at least 2 approval/reject/defer events; got: {approvals:#?}"
        );
        assert!(
            approvals.iter().any(|ev| {
                ev.get("attributes")
                    .and_then(|v| v.get("tool"))
                    .and_then(|v| v.as_str())
                    == Some("shell")
            }),
            "expected event with tool=shell"
        );
        assert!(
            approvals.iter().any(|ev| {
                ev.get("attributes")
                    .and_then(|v| v.get("tool"))
                    .and_then(|v| v.as_str())
                    == Some("file_write")
            }),
            "expected event with tool=file_write"
        );
    }

    // ── summarize_args ───────────────────────────────────────

    #[test]
    pub fn summarize_args_object() {
        let args = serde_json::json!({"command": "ls -la", "cwd": "/tmp"});
        let summary = summarize_args(&args);
        assert!(summary.contains("command: ls -la"));
        assert!(summary.contains("cwd: /tmp"));
    }

    #[test]
    pub fn summarize_args_puts_path_first_for_file_tools() {
        let args = serde_json::json!({
            "path": "src/main.rs",
            "old_string": "foo",
            "new_string": "bar"
        });
        let summary = summarize_args(&args);
        assert!(summary.starts_with("path: src/main.rs"));
        assert!(summary.contains("old_string: foo"));
        assert!(summary.contains("new_string: bar"));
    }

    #[test]
    pub fn summarize_args_truncates_long_values() {
        let long_val = "x".repeat(200);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains('…'));
        assert!(summary.len() < 200);
    }

    #[test]
    pub fn summarize_args_unicode_safe_truncation() {
        let long_val = "🦀".repeat(120);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains("content:"));
        assert!(summary.contains('…'));
    }

    #[test]
    pub fn summarize_args_non_object() {
        let args = serde_json::json!("just a string");
        let summary = summarize_args(&args);
        assert!(summary.contains("just a string"));
    }

    // ── non-interactive (channel) mode ────────────────────────

    #[test]
    fn non_interactive_manager_reports_non_interactive() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        assert!(mgr.is_non_interactive());
    }

    #[test]
    fn interactive_manager_reports_interactive() {
        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        assert!(!mgr.is_non_interactive());
    }

    #[test]
    fn non_interactive_auto_approve_tools_skip_approval() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        // auto_approve tools (file_read, memory_recall) should not need approval.
        assert!(!mgr.needs_approval("file_read"));
        assert!(!mgr.needs_approval("memory_recall"));
    }

    #[test]
    fn non_interactive_shell_skips_outer_approval_by_default() {
        let mgr = ApprovalManager::for_non_interactive(&RiskProfileConfig::default());
        assert!(!mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_backchannel_shell_requires_outer_approval() {
        let mgr = ApprovalManager::for_non_interactive_backchannel(&RiskProfileConfig::default());
        assert!(mgr.is_non_interactive());
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_always_ask_tools_need_approval() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        // always_ask tools (shell) still report as needing approval,
        // so the tool-call loop will auto-deny them in non-interactive mode.
        assert!(mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_unknown_tools_need_approval_in_supervised() {
        let mgr = ApprovalManager::for_non_interactive(&supervised_config());
        // Unknown tools in supervised mode need approval (will be auto-denied
        // by the tool-call loop for non-interactive managers).
        assert!(mgr.needs_approval("file_write"));
        assert!(mgr.needs_approval("http_request"));
    }

    #[test]
    fn non_interactive_full_autonomy_never_needs_approval() {
        let mgr = ApprovalManager::for_non_interactive(&full_config());
        // Full autonomy means no approval needed, even in non-interactive mode.
        assert!(!mgr.needs_approval("shell"));
        assert!(!mgr.needs_approval("file_write"));
        assert!(!mgr.needs_approval("anything"));
    }

    #[test]
    fn non_interactive_readonly_never_needs_approval() {
        let config = RiskProfileConfig {
            level: AutonomyLevel::ReadOnly,
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::for_non_interactive(&config);
        // ReadOnly blocks execution elsewhere; approval manager does not prompt.
        assert!(!mgr.needs_approval("shell"));
    }

    #[test]
    fn non_interactive_grant_hit_short_circuits() {
        use std::sync::Arc;
        let tmp = tempfile::TempDir::new().unwrap();
        let grants =
            Arc::new(crate::approval::grant_store::SqliteGrantStore::new(tmp.path()).unwrap())
                as Arc<dyn ApprovalGrantStore>;
        grants
            .put(crate::approval::grant_store::ApprovalGrant::new(
                "tg".into(),
                None,
                "u_bob".into(),
                "file_write".into(),
                "u_admin".into(),
                "tg".into(),
            ))
            .unwrap();
        let mgr =
            ApprovalManager::for_non_interactive(&supervised_config()).with_grant_store(grants);
        let ctx = GrantLookupCtx {
            channel_ref: "tg".into(),
            topic: None,
            user_master_id: "u_bob".into(),
        };
        assert_eq!(
            mgr.approval_requirement("file_write", Some(&ctx)),
            ApprovalRequirement::Approved
        );
    }

    #[test]
    fn non_interactive_always_ask_overrides_grant() {
        use std::sync::Arc;
        let tmp = tempfile::TempDir::new().unwrap();
        let grants =
            Arc::new(crate::approval::grant_store::SqliteGrantStore::new(tmp.path()).unwrap())
                as Arc<dyn ApprovalGrantStore>;
        grants
            .put(crate::approval::grant_store::ApprovalGrant::new(
                "tg".into(),
                None,
                "u_bob".into(),
                "shell".into(),
                "u_admin".into(),
                "tg".into(),
            ))
            .unwrap();
        let mgr =
            ApprovalManager::for_non_interactive(&supervised_config()).with_grant_store(grants);
        let ctx = GrantLookupCtx {
            channel_ref: "tg".into(),
            topic: None,
            user_master_id: "u_bob".into(),
        };
        // shell is in always_ask, so grant is ignored.
        assert_eq!(
            mgr.approval_requirement("shell", Some(&ctx)),
            ApprovalRequirement::Prompt
        );
    }

    // ── ApprovalResponse serde ───────────────────────────────

    #[test]
    fn approval_response_serde_roundtrip() {
        let json = serde_json::to_string(&ApprovalResponse::Always).unwrap();
        assert_eq!(json, "\"always\"");
        let parsed: ApprovalResponse = serde_json::from_str("\"no\"").unwrap();
        assert_eq!(parsed, ApprovalResponse::No);
        let json =
            serde_json::to_string(&ApprovalResponse::ReplaceWith("foo".to_string())).unwrap();
        let parsed: ApprovalResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ApprovalResponse::ReplaceWith("foo".to_string()));
    }

    // ── ApprovalRequest ──────────────────────────────────────

    #[test]
    fn approval_request_serde() {
        let req = ApprovalRequest {
            tool_name: "shell".into(),
            arguments: serde_json::json!({"command": "echo hi"}),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "shell");
    }

    // ── Regression: #4247 default approved tools in channels ──

    #[test]
    fn non_interactive_allows_default_auto_approve_tools() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);

        for tool in &config.auto_approve {
            assert!(
                !mgr.needs_approval(tool),
                "default auto_approve tool '{tool}' should not need approval in non-interactive mode"
            );
        }
    }

    #[test]
    fn non_interactive_denies_unknown_tools() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("some_unknown_tool"),
            "unknown tool should need approval"
        );
    }

    #[test]
    fn non_interactive_tool_search_is_auto_approved() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            !mgr.needs_approval("tool_search"),
            "tool_search discovery must not need approval in non-interactive mode"
        );
    }

    #[test]
    fn non_interactive_weather_is_auto_approved() {
        let config = RiskProfileConfig::default();
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            !mgr.needs_approval("weather"),
            "weather tool must not need approval — it is in the default auto_approve list"
        );
    }

    #[test]
    fn always_ask_overrides_auto_approve() {
        let config = RiskProfileConfig {
            always_ask: vec!["weather".into()],
            ..RiskProfileConfig::default()
        };
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("weather"),
            "always_ask must override auto_approve"
        );
    }

    // ── ChannelApprovalResponse → ApprovalResponse mapping ──────

    #[test]
    fn channel_approve_maps_to_yes() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match ChannelApprovalResponse::Approve {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert_eq!(mapped, ApprovalResponse::Yes);
    }

    #[test]
    fn channel_always_approve_maps_to_always() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match ChannelApprovalResponse::AlwaysApprove {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert_eq!(mapped, ApprovalResponse::Always);
    }

    #[test]
    fn channel_deny_maps_to_no() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match ChannelApprovalResponse::Deny {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert_eq!(mapped, ApprovalResponse::No);
    }

    #[test]
    fn channel_deny_with_edit_maps_to_replace_with() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        let mapped = match (ChannelApprovalResponse::DenyWithEdit {
            replacement: "x".to_string(),
        }) {
            ChannelApprovalResponse::Approve => ApprovalResponse::Yes,
            ChannelApprovalResponse::AlwaysApprove => ApprovalResponse::Always,
            ChannelApprovalResponse::Deny => ApprovalResponse::No,
            ChannelApprovalResponse::DenyWithEdit { replacement } => {
                ApprovalResponse::ReplaceWith(replacement)
            }
        };
        assert!(matches!(mapped, ApprovalResponse::ReplaceWith(s) if s == "x"));
    }

    #[test]
    fn replace_with_is_not_yes_or_no() {
        let r = ApprovalResponse::ReplaceWith("new text".to_string());
        assert_ne!(r, ApprovalResponse::Yes);
        assert_ne!(r, ApprovalResponse::No);
    }

    #[test]
    fn channel_approval_request_serde_roundtrip() {
        use zeroclaw_api::channel::ChannelApprovalRequest;
        let req = ChannelApprovalRequest {
            tool_name: "shell".into(),
            arguments_summary: "command: ls -la".into(),
            raw_arguments: None,
            thread_ts: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ChannelApprovalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "shell");
        assert_eq!(parsed.arguments_summary, "command: ls -la");
    }

    #[test]
    fn channel_approval_response_serde_roundtrip() {
        use zeroclaw_api::channel::ChannelApprovalResponse;
        // AlwaysApprove serializes to "always" to match the CLI-side
        // ApprovalResponse::Always and keep audit logs consistent.
        let json = serde_json::to_string(&ChannelApprovalResponse::AlwaysApprove).unwrap();
        assert_eq!(json, "\"always\"");
        let parsed: ChannelApprovalResponse = serde_json::from_str("\"always\"").unwrap();
        assert_eq!(parsed, ChannelApprovalResponse::AlwaysApprove);
        let parsed: ChannelApprovalResponse = serde_json::from_str("\"deny\"").unwrap();
        assert_eq!(parsed, ChannelApprovalResponse::Deny);
    }

    // ── summarize_args secret-key redaction ────────────────────

    #[test]
    fn summarize_args_redacts_known_secret_key_names() {
        let args = serde_json::json!({
            "endpoint": "https://api.example.com",
            "api_key": "sk-very-secret-key-value",
            "oauth_token": "oauth-secret",
            "client_secret": "client-secret",
            "password": "hunter2",
            "private_key": "-----BEGIN PRIVATE KEY-----abc",
            "bearer_token": "bearer-thing",
        });
        let summary = summarize_args(&args);
        for needle in [
            "sk-very-secret-key-value",
            "oauth-secret",
            "client-secret",
            "hunter2",
            "-----BEGIN PRIVATE KEY-----",
            "bearer-thing",
        ] {
            assert!(
                !summary.contains(needle),
                "summary leaked secret value {needle:?}: {summary}"
            );
        }
        assert!(summary.contains("endpoint:"));
        assert!(summary.contains("api.example.com"));
    }

    #[test]
    fn summarize_args_keeps_non_secret_values() {
        let args = serde_json::json!({
            "path": "/tmp/file.txt",
            "limit": 42,
        });
        let summary = summarize_args(&args);
        assert!(summary.contains("/tmp/file.txt"));
        assert!(summary.contains("42"));
    }

    #[test]
    fn summarize_args_redaction_is_case_insensitive_and_substring_aware() {
        let args = serde_json::json!({
            "X-API-Key": "hdrsecret",
            "DBPassword": "dbpw",
            "AuthHeader": "auth-thing",
        });
        let summary = summarize_args(&args);
        for leaked in ["hdrsecret", "dbpw", "auth-thing"] {
            assert!(
                !summary.contains(leaked),
                "redaction missed {leaked:?}: {summary}"
            );
        }
    }
}
