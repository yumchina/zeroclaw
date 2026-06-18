#[allow(unused_imports)]
pub use zeroclaw_runtime::approval::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RiskProfileConfig;
    use crate::security::AutonomyLevel;

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

    // ── audit log (rewritten to use zeroclaw-log capture) ─────

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
            zeroclaw_runtime::approval::decision_reason::INTERACTIVE_DENY,
            serde_json::json!({}),
        );
        mgr.record_decision(
            "file_write",
            &serde_json::json!({"path": "out.txt"}),
            &ApprovalResponse::Yes,
            "cli",
            zeroclaw_runtime::approval::decision_reason::INTERACTIVE_APPROVE,
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
                    Some("approve" | "reject")
                )
            })
            .collect();
        assert!(
            approvals.len() >= 2,
            "expected at least 2 approval/reject events; got: {approvals:#?}"
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

    #[test]
    fn record_decision_event_contains_channel_attribution() {
        let _g1 = ::zeroclaw_log::__private_test_writer_lock();
        let _g2 = ::zeroclaw_log::__private_test_hook_lock();
        let _sub = ::zeroclaw_log::try_install_capture_subscriber();
        let mut rx = ::zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let mgr = ApprovalManager::from_risk_profile(&supervised_config());
        mgr.record_decision(
            "shell",
            &serde_json::json!({"command": "ls"}),
            &ApprovalResponse::Yes,
            "telegram",
            zeroclaw_runtime::approval::decision_reason::INTERACTIVE_APPROVE,
            serde_json::json!({}),
        );

        let captured: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(
            captured.iter().any(|ev| {
                ev.get("attributes")
                    .and_then(|v| v.get("channel"))
                    .and_then(|v| v.as_str())
                    == Some("telegram")
            }),
            "expected event with channel=telegram; got: {captured:#?}"
        );
    }

    // ── summarize_args ───────────────────────────────────────

    #[test]
    fn summarize_args_object() {
        let args = serde_json::json!({"command": "ls -la", "cwd": "/tmp"});
        let summary = summarize_args(&args);
        assert!(summary.contains("command: ls -la"));
        assert!(summary.contains("cwd: /tmp"));
    }

    #[test]
    fn summarize_args_truncates_long_values() {
        let long_val = "x".repeat(200);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains('…'));
        assert!(summary.len() < 200);
    }

    #[test]
    fn summarize_args_unicode_safe_truncation() {
        let long_val = "🦀".repeat(120);
        let args = serde_json::json!({ "content": long_val });
        let summary = summarize_args(&args);
        assert!(summary.contains("content:"));
        assert!(summary.contains('…'));
    }

    #[test]
    fn summarize_args_non_object() {
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


    // ── ApprovalResponse serde ───────────────────────────────

    #[test]
    fn approval_response_serde_roundtrip() {
        let json = serde_json::to_string(&ApprovalResponse::Always).unwrap();
        assert_eq!(json, "\"always\"");
        let parsed: ApprovalResponse = serde_json::from_str("\"no\"").unwrap();
        assert_eq!(parsed, ApprovalResponse::No);
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
        let mut config = RiskProfileConfig::default();
        config.always_ask = vec!["weather".into()];
        let mgr = ApprovalManager::for_non_interactive(&config);
        assert!(
            mgr.needs_approval("weather"),
            "always_ask must override auto_approve"
        );
    }
}
