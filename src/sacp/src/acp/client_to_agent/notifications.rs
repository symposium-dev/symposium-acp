use agent_client_protocol_schema::CancelNotification;
use serde::Serialize;

use crate::jsonrpc::{JrMessage, JrNotification};
use crate::util::json_cast;

impl JrMessage for CancelNotification {
    fn into_untyped_message(self) -> Result<crate::UntypedMessage, agent_client_protocol_schema::Error> {
        let method = self.method().to_string();
        crate::UntypedMessage::new(&method, self)
    }

    fn method(&self) -> &str {
        "session/cancel"
    }

    fn parse_request(
        _method: &str,
        _params: &impl Serialize,
    ) -> Option<Result<Self, agent_client_protocol_schema::Error>> {
        // This is a notification, not a request
        None
    }

    fn parse_notification(
        method: &str,
        params: &impl Serialize,
    ) -> Option<Result<Self, agent_client_protocol_schema::Error>> {
        if method != "session/cancel" {
            return None;
        }

        Some(json_cast(params))
    }
}

impl JrNotification for CancelNotification {}
