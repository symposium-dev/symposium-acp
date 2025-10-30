//! Symposium Agent Client Protocol (SACP)
//!
//! SACP extends ACP to enable composable agent architectures through proxy chains.
//! Each proxy in the chain can intercept and transform messages, adding capabilities
//! like tool injection, context management, and protocol adaptation.

mod acp;
mod capabilities;
mod jsonrpc;
mod typed;
pub mod util;

pub use acp::*;
pub use capabilities::*;
pub use jsonrpc::*;
pub use typed::*;
