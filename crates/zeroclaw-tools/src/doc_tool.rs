//! DOC/DOCX import/export tool using office_oxide.
//!
//! - office_oxide: read .doc/.docx files, write .docx files

use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_config::policy::SecurityPolicy;

/// Maximum file size for DOC operations (50 MB).
const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
/// Maximum content items to process or export.
const MAX_CONTENT_ITEMS: usize = 1000;

pub struct DocTool {
    security: Arc<SecurityPolicy>,
    #[expect(dead_code)] // Reserved for future relative path resolution
    workspace_dir: PathBuf,
}

impl DocTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
        }
    }
}