//! Shared domain types for Continuum.
//!
//! Every other crate depends on this one; it depends on nothing internal.
//! Keeping the domain models here is what stops tree-sitter, graph, and
//! transport types from leaking across module boundaries.

pub mod dto;
pub mod error;
pub mod protocol;
pub mod settings;

pub use error::{ContinuumError, Result};
pub use protocol::{Handshake, HandshakeReply, LockFile, MCP_PROTOCOL_VERSION, PROTOCOL_VERSION};
pub use settings::{Settings, DEFAULT_MODEL_REPO};
