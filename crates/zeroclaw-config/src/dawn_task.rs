//! Dawn task type configuration.
//!
//! Maps task type IDs (1=doc extraction, 2=code analysis, ...) to the
//! channel + addressee that handles each type. Used by the
//! `dawn_create_task` / `dawn_query_task` tools to route a caller-supplied
//! task type to the right executor on the Dawn platform.
//!
//! Example TOML:
//!
//! ```toml
//! [dawn_task.1]
//! channel     = "dawnim.work"
//! recipient   = "1878_xuanji_agent"
//! name        = "璇玑文档提取"
//! description = "extract PDF/Word/PPT/Excel content"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::traits::{HasPropKind, PropKind};

/// Configuration for a single task type's executor (the channel + addressee
/// that handles tasks of this type).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnTaskExecutorConfig {
    /// Composite channel key `"<type>.<alias>"`, e.g. `"dawnim.work"`. The
    /// channel must be configured by a matching `[channels.<type>.<alias>]`
    /// block and its `Channel::send` impl must support `SendKind::TaskSubmit`
    /// / `TaskQuery`.
    pub channel: String,
    /// Channel-specific addressee:
    /// - dawnim: agent UID, e.g. `"1878_xuanji_agent"`
    /// - wechat: openid / group_id
    /// - slack: webhook URL or user/channel ID
    pub recipient: String,
    /// Human-readable name (used in logs and operator UX).
    pub name: String,
    /// Description of what this executor does (injected into the
    /// dawn_create_task tool description).
    pub description: String,
}

/// Registry mapping task type id → executor configuration.
///
/// TOML table keys are always strings, so the numeric task type is converted
/// to string at lookup time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnTaskExecutors {
    #[serde(flatten)]
    pub executors: HashMap<String, DawnTaskExecutorConfig>,
}

impl HasPropKind for DawnTaskExecutors {
    const PROP_KIND: PropKind = PropKind::Object;
}

impl DawnTaskExecutors {
    /// Look up an executor configuration by numeric task type.
    pub fn get_by_type(&self, task_type: u8) -> Option<&DawnTaskExecutorConfig> {
        self.executors.get(&task_type.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_channel_and_recipient_fields() {
        let toml = r#"
            ["1"]
            channel = "dawnim.work"
            recipient = "1878_xuanji_agent"
            name = "璇玑文档提取"
            description = "extract docs"
        "#;
        let cfg: DawnTaskExecutors = toml::from_str(toml).unwrap();
        let exec = cfg.get_by_type(1).expect("type 1 present");
        assert_eq!(exec.channel, "dawnim.work");
        assert_eq!(exec.recipient, "1878_xuanji_agent");
        assert_eq!(exec.name, "璇玑文档提取");
    }

    #[test]
    fn default_executors_collection_is_empty() {
        let cfg = DawnTaskExecutors::default();
        assert!(cfg.executors.is_empty());
        assert!(cfg.get_by_type(1).is_none());
    }

    #[test]
    fn get_by_unknown_type_returns_none() {
        let cfg = DawnTaskExecutors::default();
        assert!(cfg.get_by_type(99).is_none());
    }

    #[test]
    fn missing_channel_field_fails_to_parse() {
        let toml = r#"
            ["1"]
            recipient = "x"
            name = "n"
            description = "d"
        "#;
        let err = toml::from_str::<DawnTaskExecutors>(toml).unwrap_err();
        assert!(err.to_string().contains("channel"), "got: {err}");
    }
}
