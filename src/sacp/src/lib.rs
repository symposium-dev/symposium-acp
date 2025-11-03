//! Symposium Agent Client Protocol (SACP)
//!
//! SACP extends ACP to enable composable agent architectures through proxy chains.
//! Each proxy in the chain can intercept and transform messages, adding capabilities
//! like tool injection, context management, and protocol adaptation.

mod acp;
mod capabilities;
mod jsonrpc;
pub mod util;

// Re-export all ACP types
pub use agent_client_protocol_schema::*;

pub use acp::*;
pub use capabilities::*;
pub use jsonrpc::*;
pub use util::{TypeNotification, TypeRequest};
