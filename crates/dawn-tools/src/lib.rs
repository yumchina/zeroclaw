//! Tools integrating ZeroClaw with the Dawn SaaS.
//!
//! Currently exposes:
//! - [`s3::DawnS3Tool`] — uploads files to a Dawn S3-compatible storage endpoint.
//! - [`web_search::DawnWebSearchTool`] — searches via the internal Yumc-Search API.
//! - [`crawl::DawnCrawlTool`] — fetches full page content via the Dawn crawl service.
//! - [`task::CreateTaskTool`] / [`task::QueryTaskTool`] — submit & poll long-running
//!   tasks on the Dawn platform via the DawnIM channel.

pub mod crawl;
pub mod s3;
pub mod task;
pub mod web_search;

pub use crawl::DawnCrawlTool;
pub use s3::DawnS3Tool;
pub use task::{
    CreateTaskTool, QueryTaskTool, TASK_CONTEXT, TaskContext, TaskMessage, set_channel_bridge,
};
pub use web_search::DawnWebSearchTool;
