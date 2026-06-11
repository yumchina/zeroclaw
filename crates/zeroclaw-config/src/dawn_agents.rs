//! Dawn Agent task type configuration.
//!
//! Maps task type IDs (1=doc extraction, 2=code analysis, etc.) to their
//! corresponding WuKongIM agent UIDs and metadata.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::traits::{HasPropKind, PropKind};

/// Configuration for a single Dawn agent type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnAgentConfig {
    /// WuKongIM UID of the agent that handles this task type.
    pub uid: String,
    /// Human-readable name of this agent/task type.
    pub name: String,
    /// Description of what this agent does.
    pub description: String,
}

/// Collection of Dawn agent configurations keyed by task type ID.
///
/// The key is the task type number (1, 2, 3, etc.) as a string.
/// TOML table keys are always strings, so the numeric type is parsed at usage time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct DawnAgents {
    #[serde(flatten)]
    pub agents: HashMap<String, DawnAgentConfig>,
}

impl HasPropKind for DawnAgents {
    const PROP_KIND: PropKind = PropKind::Object;
}

impl DawnAgents {
    /// Look up an agent by numeric task type.
    pub fn get_by_type(&self, task_type: u8) -> Option<&DawnAgentConfig> {
        self.agents.get(&task_type.to_string())
    }
}

impl Default for DawnAgents {
    fn default() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }
}
