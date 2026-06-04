//! Tools integrating ZeroClaw with the Dawn SaaS.
//!
//! Currently exposes three tools:
//! - [`s3::DawnS3Tool`] — uploads files to a Dawn S3-compatible storage endpoint.
//! - [`web_search::DawnWebSearchTool`] — searches via the internal Yumc-Search API.
//! - [`crawl::DawnCrawlTool`] — fetches full page content via the Dawn crawl service.

pub mod crawl;
pub mod s3;
pub mod web_search;

pub use crawl::DawnCrawlTool;
pub use s3::DawnS3Tool;
pub use web_search::DawnWebSearchTool;
