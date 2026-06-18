//! OrchestratorChannelDirectory — adapter for ApprovalBroker's ChannelDirectory trait.

use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw_api::channel::Channel;
use zeroclaw_runtime::approval::ChannelDirectory;

pub struct OrchestratorChannelDirectory {
    channels: Arc<HashMap<String, Arc<dyn Channel>>>,
}

impl OrchestratorChannelDirectory {
    pub fn new(channels: Arc<HashMap<String, Arc<dyn Channel>>>) -> Self {
        Self { channels }
    }
}

impl ChannelDirectory for OrchestratorChannelDirectory {
    fn lookup(&self, channel_ref: &str) -> Option<Arc<dyn Channel>> {
        self.channels.get(channel_ref).cloned()
    }
}
