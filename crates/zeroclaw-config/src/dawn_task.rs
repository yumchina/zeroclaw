//! Dawn task type configuration.
//!
//! Maps task type IDs (1=doc extraction, 2=code analysis, etc.) to the
//! DawnIM agent UID that handles each type, plus human-readable metadata.
//! Used by the `dawn_create_task` and `dawn_query_task` tools to route a
//! caller-supplied task type to the right Agent on the Dawn platform.
//!
//! Example TOML:
//!
//! ```toml
//! [dawn_task.1]
//! uid = "1878_xuanji_agent"
//! name = "璇玑文档提取"
//! description = "提取 PDF/Word/PPT/Excel 等文档内容"
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::traits::{HasPropKind, PropKind};

/// Configuration for a single Dawn task type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnTaskConfig {
    /// DawnIM UID of the agent that handles this task type.
    pub uid: String,
    /// Human-readable name of this task type.
    pub name: String,
    /// Description of what this task does.
    pub description: String,
}

/// Collection of Dawn task configurations keyed by task type ID.
///
/// The key is the task type number (1, 2, 3, ...) as a string. TOML table
/// keys are always strings, so the numeric type is parsed at lookup time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnTasks {
    #[serde(flatten)]
    pub tasks: HashMap<String, DawnTaskConfig>,
}

impl HasPropKind for DawnTasks {
    const PROP_KIND: PropKind = PropKind::Object;
}

impl DawnTasks {
    /// Look up a task entry by numeric task type. Returns `None` when no
    /// entry is configured for `task_type`.
    pub fn get_by_type(&self, task_type: u8) -> Option<&DawnTaskConfig> {
        self.tasks.get(&task_type.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_toml_table() {
        // With `#[serde(flatten)]`, the HashMap keys live directly on the
        // top-level struct, so the TOML under `[dawn_task]` (in the full
        // Config) looks like `[dawn_task.<type>]` — here we parse a
        // standalone `DawnTasks` so the type key is `["1"]`.
        let toml = r#"
            ["1"]
            uid = "1878_xuanji_agent"
            name = "璇玑文档提取"
            description = "extract PDFs"
        "#;
        let cfg: DawnTasks = toml::from_str(toml).unwrap();
        let task = cfg.get_by_type(1).expect("type 1 present");
        assert_eq!(task.uid, "1878_xuanji_agent");
        assert_eq!(task.name, "璇玑文档提取");
    }

    #[test]
    fn get_by_unknown_type_returns_none() {
        let cfg = DawnTasks::default();
        assert!(cfg.get_by_type(99).is_none());
    }

    #[test]
    fn default_is_empty() {
        let cfg = DawnTasks::default();
        assert!(cfg.tasks.is_empty());
        assert!(cfg.get_by_type(1).is_none());
    }
}
