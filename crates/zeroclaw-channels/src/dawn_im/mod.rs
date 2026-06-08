//! DawnIM channel implementation for ZeroClaw.
//!
//! Module layout (responsibility-oriented):
//! - [`connection`] — WebSocket connection + JSON-RPC 2.0 protocol types
//! - [`messaging`]  — message encoding, media download (image/file)
//! - [`filter`]     — permission allowlist + @mention parsing
//! - [`approval`]   — tool-call approval flow (PendingApprovals + card UI)
//! - [`channel`]    — main [`DawnIMChannel`] struct + [`Channel`] trait impl

pub mod approval;
pub mod channel;
pub mod connection;
pub mod exception_card;
pub mod filter;
pub mod messaging;

pub use channel::DawnIMChannel;
