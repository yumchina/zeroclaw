//! Tools integrating ZeroClaw with the Dawn SaaS.
//!
//! Currently exposes a single tool, [`s3::DawnS3Tool`], for uploading
//! files to a Dawn S3-compatible storage endpoint. Future Dawn-backed
//! tools live alongside it under their own sub-modules.

pub mod s3;
pub mod web_search;

pub use s3::DawnS3Tool;
