//! Stable reason strings for approval audit events. Keep in sync with the spec table
//! at docs/superpowers/specs/2026-06-18-persistent-tool-approval-grants-design.md §8.4.

pub const INTERACTIVE_APPROVE: &str = "interactive_approve";
pub const INTERACTIVE_ALWAYS: &str = "interactive_always";
pub const INTERACTIVE_DENY: &str = "interactive_deny";
pub const INTERACTIVE_REPLACE: &str = "interactive_replace";
pub const CACHED_GRANT: &str = "cached_grant";
pub const ALL_SUPERUSERS_TIMEOUT: &str = "all_superusers_timeout";
pub const ALL_CHANNELS_FAILED: &str = "all_channels_failed";
pub const NO_SUPERUSER_CONFIGURED: &str = "no_superuser_configured";
pub const NO_MASTER_CHANNEL: &str = "no_master_channel";
pub const POLICY_AUTO_APPROVE: &str = "policy_auto_approve";
pub const POLICY_AUTONOMY_FULL: &str = "policy_autonomy_full";
