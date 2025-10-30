mod notifications;
mod requests;

/// Requests that agents sent to clients via the ACP protocol.
pub use agent_client_protocol_schema::AgentRequest as AcpAgentToClientRequest;

/// Notifications that agents sent to clients via the ACP protocol.
pub use agent_client_protocol_schema::AgentNotification as AcpAgentToClientNotification;
