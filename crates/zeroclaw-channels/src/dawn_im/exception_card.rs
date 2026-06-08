//! Exception scene card for DawnIM.
//!
//! Renders non-normal task endings (loop/timeout/error/context/cancel/interrupt)
//! as a localized display card. Distinct from `approval.rs` (tool-call approval
//! flow); reuses only `WkAction` for the reserved `actions` field.

use serde::{Deserialize, Serialize};

use super::approval::WkAction;
use super::connection::WkMessageType;

/// Display-only exception card. `actions` is reserved for a future interactive
/// Human Takeover phase and is `None` in this phase.
#[derive(Debug, Serialize, Deserialize)]
pub struct WkExceptionCard {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub kind: String,
    pub level: String,
    pub heading: String,
    pub reason: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<WkAction>>,
}

/// Known error codes (without the `ERR:` prefix). Unknown codes fall back to
/// `step_error`.
const KNOWN_CODES: &[&str] = &[
    "loop_detected",
    "step_timeout",
    "step_error",
    "context_window_exceeded",
    "cancelled",
    "interrupted",
];

/// Build an exception card for an `ERR:` code (prefix already stripped).
/// Looks up localized heading/reason/detail via the runtime i18n catalogue.
pub fn build_exception_card(code: &str) -> WkExceptionCard {
    let code = if KNOWN_CODES.contains(&code) {
        code
    } else {
        "step_error"
    };
    let level = match code {
        "cancelled" | "interrupted" => "cancelled",
        _ => "error",
    };
    let key_code = code.replace('_', "-");
    let heading = zeroclaw_runtime::i18n::get_error_string(&format!("error-heading-{level}"))
        .unwrap_or_default();
    let reason = zeroclaw_runtime::i18n::get_error_string(&format!("error-{key_code}-reason"))
        .unwrap_or_default();
    let detail = zeroclaw_runtime::i18n::get_error_string(&format!("error-{key_code}-detail"))
        .unwrap_or_default();
    WkExceptionCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        kind: code.to_string(),
        level: level.to_string(),
        heading,
        reason,
        detail,
        actions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_card_sets_kind_level_and_type() {
        let card = build_exception_card("step_timeout");
        assert_eq!(card.msg_type, WkMessageType::INTERACTIVE_CARD);
        assert_eq!(card.kind, "step_timeout");
        assert_eq!(card.level, "error");
        assert!(card.actions.is_none());
        // i18n value present (en default in test env)
        assert!(!card.reason.is_empty(), "reason should be populated");
        assert!(!card.detail.is_empty(), "detail should be populated");
        assert!(!card.heading.is_empty(), "heading should be populated");
    }

    #[test]
    fn cancelled_and_interrupted_are_cancelled_level() {
        assert_eq!(build_exception_card("cancelled").level, "cancelled");
        assert_eq!(build_exception_card("interrupted").level, "cancelled");
    }

    #[test]
    fn unknown_code_falls_back_to_step_error() {
        let card = build_exception_card("totally_unknown");
        assert_eq!(card.kind, "step_error");
        assert_eq!(card.level, "error");
    }

    #[test]
    fn card_serializes_with_type_and_fields() {
        let card = build_exception_card("cancelled");
        let v = serde_json::to_value(&card).expect("serialize");
        assert_eq!(v["type"], 20); // INTERACTIVE_CARD
        assert_eq!(v["kind"], "cancelled");
        assert_eq!(v["level"], "cancelled");
        assert!(v.get("heading").is_some());
        assert!(v.get("reason").is_some());
        assert!(v.get("detail").is_some());
        // actions=None must be omitted (skip_serializing_if)
        assert!(v.get("actions").is_none());
    }
}
